//! HTTP/1 file serving and opt-in uploads through the shared rooted filesystem.
//!
//! Files are streamed, directory listings are limited to 10,000 entries, and every
//! accepted connection belongs to the server's task set. Regular files (including
//! rooted internal symlink targets) and directories are exposed; special files
//! and directory symlinks are refused.
//! Single byte ranges are supported. Multipart ranges and unknown range units are
//! ignored, and conditional requests/validators are not implemented.
//! PUT creates one file; multipart POST uploads one file into an existing directory.
//! Uploads are streamed, size-limited and published without replacing existing entries.

use crate::fs::{DirEntry, FsError, NodeKind, SharedFs, Upload};
use crate::{Endpoint, EventSender, HttpConfig, Protocol, ServerHandle, Transport, emit};
use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, header, uri::Authority};
use axum::response::Response;
use http_body_util::BodyExt;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const MAX_MULTIPART_OVERHEAD: u64 = 64 * 1024;

#[derive(Clone)]
struct AppState {
    fs: Arc<SharedFs>,
    events: EventSender,
    allow_upload: bool,
    max_upload_bytes: u64,
}

/// Bind the configured address before returning a handle owning all connections.
/// `enabled` is a frontend selection flag and does not suppress an explicit start.
pub async fn start(
    fs: Arc<SharedFs>,
    config: HttpConfig,
    events: EventSender,
) -> Result<ServerHandle> {
    ensure!(
        (1..=4096).contains(&config.max_connections),
        "HTTP max_connections must be 1–4096"
    );
    ensure!(config.max_upload_bytes > 0, "invalid HTTP upload limit");
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding HTTP to {}", config.bind))?;
    let address = listener
        .local_addr()
        .context("reading HTTP listen address")?;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let body_limit = usize::try_from(
        config
            .max_upload_bytes
            .saturating_add(MAX_MULTIPART_OVERHEAD),
    )
    .unwrap_or(usize::MAX);
    let app = Router::new()
        .fallback(request)
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(AppState {
            fs,
            events: events.clone(),
            allow_upload: config.allow_upload,
            max_upload_bytes: config.max_upload_bytes,
        });
    emit(
        &events,
        Protocol::Http,
        format!("Listening on {address}"),
        false,
    );
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        let outcome = loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break Ok(()),
                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result {
                        emit(&events, Protocol::Http, format!("Connection task failed: {error}"), true);
                    }
                }
                accepted = listener.accept(), if connections.len() < config.max_connections => {
                    let (stream, peer) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => break Err(error).context("accepting HTTP connection"),
                    };
                    let service = TowerToHyperService::new(app.clone());
                    let connection_stop = stop.clone();
                    let connection_events = events.clone();
                    connections.spawn(async move {
                        tokio::select! {
                            biased;
                            _ = connection_stop.cancelled() => {}
                            result = http1::Builder::new()
                                .max_buf_size(32 * 1024)
                                .serve_connection(TokioIo::new(stream), service) => {
                                if let Err(error) = result {
                                    emit(&connection_events, Protocol::Http,
                                        format!("Connection {peer}: {error}"), true);
                                }
                            }
                        }
                    });
                }
            }
        };
        drop(listener);
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        emit(&events, Protocol::Http, "Stopped", outcome.is_err());
        outcome
    });
    Ok(ServerHandle::new(
        Protocol::Http,
        vec![Endpoint {
            role: "http",
            transport: Transport::Tcp,
            address,
        }],
        cancel,
        task,
    ))
}

async fn request(State(state): State<AppState>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let response = dispatch(&state, request).await;
    emit(
        &state.events,
        Protocol::Http,
        format!("{} {}: {}", method, uri.path(), response.status()),
        response.status().is_server_error(),
    );
    response
}

async fn dispatch(state: &AppState, request: Request<Body>) -> Response {
    let method = request.method();
    let head = method == Method::HEAD;
    let write = method == Method::PUT || method == Method::POST;
    if method != Method::GET && !head && (!write || !state.allow_upload) {
        let mut response = message(
            StatusCode::METHOD_NOT_ALLOWED,
            "Method not allowed\n",
            false,
        );
        let allowed = if state.allow_upload {
            "GET, HEAD, PUT, POST"
        } else {
            "GET, HEAD"
        };
        response
            .headers_mut()
            .insert(header::ALLOW, allowed.parse().unwrap());
        return response;
    }
    let path = match decode_path(request.uri().path()) {
        Ok(path) => path,
        Err(status) => return message(status, "Unsafe or malformed path\n", head),
    };
    if !write {
        return serve(state, path, request.headers(), head).await;
    }
    if !same_origin(request.headers()) {
        return upload_message(StatusCode::FORBIDDEN);
    }
    let result = if method == Method::PUT {
        if path.relative.as_os_str().is_empty() || request.uri().path().ends_with('/') {
            return upload_message(StatusCode::BAD_REQUEST);
        }
        upload_put(state, path, request).await
    } else {
        upload_form(state, path, request).await
    };
    result.unwrap_or_else(upload_message)
}

fn same_origin(headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site")
    {
        return false;
    }
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return true;
    };
    if origins.next().is_some() {
        return false;
    }
    let Some(origin) = origin
        .to_str()
        .ok()
        .and_then(|value| value.parse::<axum::http::Uri>().ok())
    else {
        return false;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Authority>().ok())
    else {
        return false;
    };
    origin.scheme_str() == Some("http")
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.authority().is_some_and(|authority| {
            authority.host().eq_ignore_ascii_case(host.host())
                && authority.port_u16().unwrap_or(80) == host.port_u16().unwrap_or(80)
        })
}

// Field order closes the writer before Upload removes its private temporary entry.
struct PendingUpload {
    writer: tokio::fs::File,
    upload: Upload,
    bytes: u64,
}

impl PendingUpload {
    async fn begin(state: &AppState, path: PathBuf) -> std::result::Result<Self, StatusCode> {
        let fs = state.fs.clone();
        tokio::task::spawn_blocking(move || {
            let upload = fs.begin_upload(&path)?;
            let writer = tokio::fs::File::from_std(upload.writer()?);
            Ok::<_, FsError>(Self {
                writer,
                upload,
                bytes: 0,
            })
        })
        .await
        .map_err(|error| upload_status(state, FsError::State(error.to_string())))?
        .map_err(|error| upload_status(state, error))
    }

    async fn write(
        &mut self,
        state: &AppState,
        chunk: &[u8],
    ) -> std::result::Result<(), StatusCode> {
        self.bytes = self
            .bytes
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= state.max_upload_bytes)
            .ok_or(StatusCode::PAYLOAD_TOO_LARGE)?;
        self.writer
            .write_all(chunk)
            .await
            .map_err(|error| upload_status(state, error.into()))
    }

    async fn commit(self, state: &AppState) -> std::result::Result<(), StatusCode> {
        let Self {
            mut writer, upload, ..
        } = self;
        writer
            .flush()
            .await
            .map_err(|error| upload_status(state, error.into()))?;
        drop(writer);
        tokio::task::spawn_blocking(move || upload.commit())
            .await
            .map_err(|error| upload_status(state, FsError::State(error.to_string())))?
            .map_err(|error| upload_status(state, error))
    }
}

fn upload_status(state: &AppState, error: FsError) -> StatusCode {
    let status = fs_status(error.kind());
    if status.is_server_error() {
        emit(
            &state.events,
            Protocol::Http,
            format!("Upload failed: {error}"),
            true,
        );
    }
    status
}

fn upload_message(status: StatusCode) -> Response {
    let text = match status {
        StatusCode::BAD_REQUEST => "Invalid or incomplete upload request\n",
        StatusCode::FORBIDDEN => "Upload request is not permitted\n",
        StatusCode::NOT_FOUND => "Upload directory not found\n",
        StatusCode::CONFLICT => "Target already exists; files cannot be overwritten\n",
        StatusCode::PAYLOAD_TOO_LARGE => "Upload exceeds the size limit\n",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            "Use multipart/form-data to upload into a directory\n"
        }
        _ => "Upload failed\n",
    };
    message(status, text, false)
}

fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

async fn upload_put(
    state: &AppState,
    path: ResolvedPath,
    request: Request<Body>,
) -> std::result::Result<Response, StatusCode> {
    if request.headers().contains_key(header::CONTENT_RANGE) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if declared_length(request.headers()).is_some_and(|size| size > state.max_upload_bytes) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let mut upload = PendingUpload::begin(state, path.relative).await?;
    let mut body = request.into_body();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| StatusCode::BAD_REQUEST)?;
        if let Ok(chunk) = frame.into_data() {
            upload.write(state, &chunk).await?;
        }
    }
    upload.commit(state).await?;
    let mut response = message(StatusCode::CREATED, "Upload complete\n", false);
    response
        .headers_mut()
        .insert(header::LOCATION, path.encoded.parse().unwrap());
    Ok(response)
}

async fn upload_form(
    state: &AppState,
    mut path: ResolvedPath,
    request: Request<Body>,
) -> std::result::Result<Response, StatusCode> {
    let multipart = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("multipart/form-data"));
    if !multipart {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    if declared_length(request.headers()).is_some_and(|size| {
        size > state
            .max_upload_bytes
            .saturating_add(MAX_MULTIPART_OVERHEAD)
    }) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut field = multipart
        .next_field()
        .await
        .map_err(multipart_status)?
        .ok_or(StatusCode::BAD_REQUEST)?;
    if field.name() != Some("file") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let name = field
        .file_name()
        .filter(|name| safe_component(name.as_bytes()))
        .ok_or(StatusCode::BAD_REQUEST)?;
    path.relative.push(name);
    let mut upload = PendingUpload::begin(state, path.relative).await?;
    while let Some(chunk) = field.chunk().await.map_err(multipart_status)? {
        upload.write(state, &chunk).await?;
    }
    drop(field);
    // Reject extra parts before publication, even after a valid first file.
    if multipart
        .next_field()
        .await
        .map_err(multipart_status)?
        .is_some()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    upload.commit(state).await?;
    if path.encoded != "/" {
        path.encoded.push('/');
    }
    let mut response = message(StatusCode::SEE_OTHER, "Upload complete\n", false);
    response
        .headers_mut()
        .insert(header::LOCATION, path.encoded.parse().unwrap());
    Ok(response)
}

fn multipart_status(error: axum::extract::multipart::MultipartError) -> StatusCode {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    }
}

struct ResolvedPath {
    relative: PathBuf,
    encoded: String,
}

fn decode_path(raw: &str) -> std::result::Result<ResolvedPath, StatusCode> {
    if !raw.starts_with('/') || raw.starts_with("//") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut relative = PathBuf::new();
    let mut encoded = String::from("/");
    let mut segments = raw[1..].split('/').peekable();
    while let Some(segment) = segments.next() {
        if segment.is_empty() {
            if segments.peek().is_none() {
                continue;
            }
            return Err(StatusCode::BAD_REQUEST);
        }
        let bytes = decode_segment(segment).ok_or(StatusCode::BAD_REQUEST)?;
        if !safe_component(&bytes) {
            return Err(StatusCode::FORBIDDEN);
        }
        let name = os_name(bytes).ok_or(StatusCode::BAD_REQUEST)?;
        if encoded.len() > 1 {
            encoded.push('/');
        }
        encoded.push_str(&encode_name(&name).ok_or(StatusCode::BAD_REQUEST)?);
        relative.push(name);
    }
    Ok(ResolvedPath { relative, encoded })
}

fn decode_segment(segment: &str) -> Option<Vec<u8>> {
    let mut result = Vec::with_capacity(segment.len());
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let high = char::from(*bytes.get(i + 1)?).to_digit(16)?;
            let low = char::from(*bytes.get(i + 2)?).to_digit(16)?;
            result.push((high * 16 + low) as u8);
            i += 3;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    Some(result)
}

fn safe_component(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes != b"."
        && bytes != b".."
        && !bytes
            .iter()
            .any(|b| matches!(*b, 0..=31 | 127 | b'/' | b'\\' | b':'))
        && !bytes.ends_with(b".")
        && !bytes.ends_with(b" ")
        && !bytes
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b".filemux-"))
}

#[cfg(unix)]
fn os_name(bytes: Vec<u8>) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Some(OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn os_name(bytes: Vec<u8>) -> Option<OsString> {
    String::from_utf8(bytes).ok().map(OsString::from)
}

#[cfg(unix)]
fn encode_name(name: &OsStr) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    Some(percent_encode(name.as_bytes(), NON_ALPHANUMERIC).to_string())
}

#[cfg(not(unix))]
fn encode_name(name: &OsStr) -> Option<String> {
    Some(percent_encode(name.to_str()?.as_bytes(), NON_ALPHANUMERIC).to_string())
}

enum Resource {
    File(std::fs::File, u64),
    Directory(Vec<DirEntry>),
    Refused,
}

async fn serve(state: &AppState, path: ResolvedPath, headers: &HeaderMap, head: bool) -> Response {
    let fs = state.fs.clone();
    let relative = path.relative;
    let resource = tokio::task::spawn_blocking(move || -> crate::fs::Result<Resource> {
        let node = fs.lookup_path(&relative)?;
        match node.kind {
            NodeKind::File | NodeKind::Symlink => {
                let file = fs.open_read(&relative)?;
                // Derive headers from the exact opened capability, never the lstat.
                let metadata = file.metadata()?;
                if !metadata.is_file() {
                    return Ok(Resource::Refused);
                }
                Ok(Resource::File(file, metadata.len()))
            }
            NodeKind::Directory => Ok(Resource::Directory(fs.list(node.id)?)),
            _ => Ok(Resource::Refused),
        }
    })
    .await;
    match resource {
        Ok(Ok(Resource::File(file, size))) => {
            serve_file(file, size, headers, head, &state.events).await
        }
        Ok(Ok(Resource::Directory(entries))) => directory(
            entries,
            &path.encoded,
            head,
            state.allow_upload.then_some(state.max_upload_bytes),
        ),
        Ok(Ok(Resource::Refused)) => message(
            StatusCode::FORBIDDEN,
            "Only regular files and directories are served\n",
            head,
        ),
        Ok(Err(error)) => {
            let status = fs_status(error.kind());
            if status.is_server_error() {
                emit(
                    &state.events,
                    Protocol::Http,
                    format!("Filesystem request failed: {error}"),
                    true,
                );
            }
            message(status, "Filesystem request refused\n", head)
        }
        Err(error) => {
            emit(
                &state.events,
                Protocol::Http,
                format!("Filesystem task failed: {error}"),
                true,
            );
            message(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Filesystem unavailable\n",
                head,
            )
        }
    }
}

fn fs_status(kind: ErrorKind) -> StatusCode {
    match kind {
        ErrorKind::NotFound | ErrorKind::NotADirectory => StatusCode::NOT_FOUND,
        ErrorKind::PermissionDenied | ErrorKind::Unsupported | ErrorKind::IsADirectory => {
            StatusCode::FORBIDDEN
        }
        ErrorKind::InvalidInput | ErrorKind::InvalidData => StatusCode::BAD_REQUEST,
        ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn serve_file(
    file: std::fs::File,
    size: u64,
    headers: &HeaderMap,
    head: bool,
    events: &EventSender,
) -> Response {
    let mut file = tokio::fs::File::from_std(file);
    // Without a strong validator, If-Range must fall back to the full representation.
    let selected = if !head && !headers.contains_key(header::IF_RANGE) {
        select_range(headers, size)
    } else {
        Ok(None)
    };
    let (status, start, length) = match selected {
        Ok(Some((start, end))) => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
        Ok(None) => (StatusCode::OK, 0, size),
        Err(()) => {
            let mut response = message(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "Unsatisfiable byte range\n",
                head,
            );
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes */{size}").parse().unwrap(),
            );
            response
                .headers_mut()
                .insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
            return response;
        }
    };
    if !head && start != 0 {
        if let Err(error) = file.seek(std::io::SeekFrom::Start(start)).await {
            emit(
                events,
                Protocol::Http,
                format!("File seek failed: {error}"),
                true,
            );
            return message(fs_status(error.kind()), "File read unavailable\n", false);
        }
    }
    let body = if head {
        Body::empty()
    } else {
        Body::from_stream(ReaderStream::new(file.take(length)))
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        "application/octet-stream".parse().unwrap(),
    );
    headers.insert(header::CONTENT_LENGTH, length.into());
    headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{size}", start, start + length - 1)
                .parse()
                .unwrap(),
        );
    }
    response
}

fn select_range(headers: &HeaderMap, size: u64) -> std::result::Result<Option<(u64, u64)>, ()> {
    let mut ranges = headers.get_all(header::RANGE).iter();
    let Some(value) = ranges.next() else {
        return Ok(None);
    };
    if ranges.next().is_some() {
        return Ok(None);
    }
    let value = value.to_str().map_err(|_| ())?;
    let Some((unit, value)) = value.trim().split_once('=') else {
        return Err(());
    };
    if !unit.eq_ignore_ascii_case("bytes") || value.contains(',') {
        return Ok(None);
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    if size == 0 {
        return Err(());
    }
    let decimal = |text: &str| -> std::result::Result<u64, ()> {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(());
        }
        text.parse().map_err(|_| ())
    };
    if start.is_empty() {
        let suffix = decimal(end)?;
        if suffix == 0 {
            return Err(());
        }
        return Ok(Some((size.saturating_sub(suffix), size - 1)));
    }
    let start = decimal(start)?;
    let end = if end.is_empty() {
        size - 1
    } else {
        decimal(end)?.min(size - 1)
    };
    if start >= size || end < start {
        return Err(());
    }
    Ok(Some((start, end)))
}

fn directory(
    mut entries: Vec<DirEntry>,
    encoded_path: &str,
    head: bool,
    upload_limit: Option<u64>,
) -> Response {
    if entries.len() > MAX_DIRECTORY_ENTRIES {
        return message(
            StatusCode::INSUFFICIENT_STORAGE,
            "Directory exceeds listing limit\n",
            head,
        );
    }
    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    let mut html = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Filemux directory</title></head><body><h1>Directory</h1>",
    );
    if let Some(limit) = upload_limit {
        let slash = if encoded_path == "/" { "" } else { "/" };
        html.push_str(&format!(
            "<form method=\"post\" action=\"{encoded_path}{slash}\" enctype=\"multipart/form-data\">\
             <p>上传文件（最大 {limit} 字节，不覆盖已有文件）</p>\
             <input type=\"file\" name=\"file\" aria-label=\"选择文件\" required>\
             <button type=\"submit\">上传</button></form>"
        ));
    }
    html.push_str("<ul>");
    if encoded_path != "/" {
        let parent = encoded_path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        html.push_str(&format!(
            "<li><a href=\"{}/\">../</a></li>\n",
            escape_html(parent)
        ));
    }
    let prefix = if encoded_path == "/" {
        ""
    } else {
        encoded_path
    };
    for entry in entries {
        if !matches!(
            entry.node.kind,
            NodeKind::File | NodeKind::Directory | NodeKind::Symlink
        ) {
            continue;
        }
        let Some(name) = encode_name(&entry.name) else {
            continue;
        };
        let Some(bytes) = decode_segment(&name) else {
            continue;
        };
        if !safe_component(&bytes) {
            continue;
        }
        let slash = if entry.node.kind == NodeKind::Directory {
            "/"
        } else {
            ""
        };
        html.push_str(&format!(
            "<li><a href=\"{prefix}/{name}{slash}\">{}{slash}</a></li>\n",
            escape_html(&entry.name.to_string_lossy())
        ));
    }
    html.push_str("</ul></body></html>\n");
    let length = html.len();
    let mut response = Response::new(if head {
        Body::empty()
    } else {
        Body::from(html)
    });
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "text/html; charset=utf-8".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, length.into());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"
            .parse()
            .unwrap(),
    );
    response
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

fn message(status: StatusCode, text: &'static str, head: bool) -> Response {
    let mut response = Response::new(if head {
        Body::empty()
    } else {
        Body::from(text)
    });
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "text/plain; charset=utf-8".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, text.len().into());
    response
}
