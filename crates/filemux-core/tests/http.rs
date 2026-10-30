use filemux_core::fs::SharedFs;
use filemux_core::{HttpConfig, ServerHandle};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Client, StatusCode, header};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Fixture {
    root: TempDir,
    _state: TempDir,
    fs: Arc<SharedFs>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let fs = Arc::new(SharedFs::open(root.path(), state.path()).unwrap());
        Self {
            root,
            _state: state,
            fs,
        }
    }

    async fn start(&self, max_connections: usize) -> ServerHandle {
        self.start_with_config(HttpConfig {
            enabled: false,
            bind: "127.0.0.1:0".parse().unwrap(),
            max_connections,
            ..HttpConfig::default()
        })
        .await
    }

    async fn start_uploads(&self, max_upload_bytes: u64) -> ServerHandle {
        self.start_with_config(HttpConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            allow_upload: true,
            max_upload_bytes,
            ..HttpConfig::default()
        })
        .await
    }

    async fn start_with_config(&self, config: HttpConfig) -> ServerHandle {
        let (events, _) = tokio::sync::broadcast::channel(32);
        filemux_core::http::start(self.fs.clone(), config, None, events)
            .await
            .unwrap()
    }
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn multipart(parts: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (field, filename, contents) in parts {
        body.extend_from_slice(
            format!(
                "--filemux-test\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(contents);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--filemux-test--\r\n");
    body
}

async fn read_status(mut socket: TcpStream) -> u16 {
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    std::str::from_utf8(&response)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

fn temporary_upload_count(directory: &std::path::Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".filemux-upload-")
        })
        .count()
}

async fn wait_for_upload_count(directory: &std::path::Path, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while temporary_upload_count(directory) != expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn streams_files_head_and_single_byte_ranges() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.path().join("data.bin"), b"0123456789").unwrap();
    std::fs::write(fixture.root.path().join("empty"), b"").unwrap();
    let server = fixture.start(8).await;
    let base = format!("http://{}", server.endpoints[0].address);
    let client = client();
    let url = format!("{base}/data.bin");

    let response = client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"0123456789");

    let response = client.head(&url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
    assert!(response.bytes().await.unwrap().is_empty());

    for (range, content_range, expected) in [
        ("bytes=2-4", "bytes 2-4/10", "234"),
        ("bytes=7-", "bytes 7-9/10", "789"),
        ("bytes=-3", "bytes 7-9/10", "789"),
        ("bytes=8-99", "bytes 8-9/10", "89"),
        ("bytes=-99", "bytes 0-9/10", "0123456789"),
    ] {
        let response = client
            .get(&url)
            .header(header::RANGE, range)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(response.headers()[header::CONTENT_RANGE], content_range);
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            expected.len().to_string()
        );
        assert_eq!(response.text().await.unwrap(), expected);
    }

    for range in ["bytes=10-", "bytes=5-2", "bytes=-0", "bytes=abc-def"] {
        let response = client
            .get(&url)
            .header(header::RANGE, range)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::RANGE_NOT_SATISFIABLE,
            "{range}"
        );
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */10");
    }
    let response = client
        .get(format!("{base}/empty"))
        .header(header::RANGE, "bytes=0-")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */0");

    // No validator is advertised: If-Range cannot authorize a partial response.
    let response = client
        .get(&url)
        .header(header::RANGE, "bytes=1-2")
        .header(header::IF_RANGE, "Wed, 21 Oct 2015 07:28:00 GMT")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"0123456789");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn directory_links_escape_display_and_roundtrip_encoded_names() {
    let fixture = Fixture::new();
    let name = "a&' #你好.txt";
    std::fs::write(fixture.root.path().join(name), b"encoded filename").unwrap();
    std::fs::create_dir(fixture.root.path().join("sub dir")).unwrap();
    std::fs::write(fixture.root.path().join(".filemux-private"), b"hidden").unwrap();
    let server = fixture.start(8).await;
    let base = format!("http://{}", server.endpoints[0].address);
    let client = client();

    let listing = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(listing.status(), StatusCode::OK);
    assert_eq!(
        listing.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    let listing = listing.text().await.unwrap();
    let encoded = utf8_percent_encode(name, NON_ALPHANUMERIC).to_string();
    assert!(listing.contains("a&amp;&#39; #你好.txt"), "{listing}");
    assert!(
        listing.contains(&format!("href=\"/{encoded}\"")),
        "{listing}"
    );
    assert!(!listing.contains(".filemux-private"));
    let response = client
        .get(format!("{base}/{encoded}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        b"encoded filename"
    );

    let listing = client
        .get(format!("{base}/sub%20dir"))
        .send()
        .await
        .unwrap();
    assert_eq!(listing.status(), StatusCode::OK);
    assert!(listing.text().await.unwrap().contains("href=\"/\">../</a>"));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejects_writes_reserved_names_and_unsafe_paths() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.path().join("keep.txt"), b"unchanged").unwrap();
    std::fs::write(fixture.root.path().join(".filemux-upload"), b"unpublished").unwrap();
    let server = fixture.start(8).await;
    let base = format!("http://{}", server.endpoints[0].address);
    let client = client();
    for method in [
        reqwest::Method::PUT,
        reqwest::Method::POST,
        reqwest::Method::DELETE,
        reqwest::Method::PATCH,
    ] {
        let response = client
            .request(method, format!("{base}/keep.txt"))
            .body("replace")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "GET, HEAD");
    }
    assert_eq!(
        std::fs::read(fixture.root.path().join("keep.txt")).unwrap(),
        b"unchanged"
    );
    for path in [
        "/.filemux-upload",
        "/%2Efilemux-upload",
        "/%2e%2e%2foutside",
        "/C%3A/secret",
        "/%5C%5Chost/share",
        "/keep.txt%3Astream",
        "/%00",
    ] {
        let response = client.get(format!("{base}{path}")).send().await.unwrap();
        assert!(
            matches!(
                response.status(),
                StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST
            ),
            "{path}: {}",
            response.status()
        );
    }
    let response = client.get(format!("{base}/missing")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // URL libraries normalize literal dot segments; exercise the wire path directly.
    let mut socket = TcpStream::connect(server.endpoints[0].address)
        .await
        .unwrap();
    socket
        .write_all(b"GET /../outside HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 403"));
    server.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn internal_file_symlinks_work_but_external_targets_are_refused() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"outside secret").unwrap();
    symlink(
        outside.path().join("secret"),
        fixture.root.path().join("link"),
    )
    .unwrap();
    std::fs::write(fixture.root.path().join("kernel.bin"), b"boot kernel").unwrap();
    symlink("kernel.bin", fixture.root.path().join("kernel")).unwrap();
    let server = fixture.start(4).await;
    let response = client()
        .get(format!("http://{}/kernel", server.endpoints[0].address))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "11");
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"boot kernel");
    let response = client()
        .get(format!("http://{}/link", server.endpoints[0].address))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!response.text().await.unwrap().contains("outside secret"));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_closes_an_active_connection_and_releases_listener() {
    let fixture = Fixture::new();
    let server = fixture.start(1).await;
    let address = server.endpoints[0].address;
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
    server.shutdown().await.unwrap();
    let mut byte = [0_u8; 1];
    let result = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    let rebound = tokio::net::TcpListener::bind(address).await.unwrap();
    drop(rebound);
}

#[tokio::test]
async fn put_uploads_roundtrip_encoded_paths_and_never_replace_existing_entries() {
    let fixture = Fixture::new();
    let directory = "子 目录";
    let filename = "a #你好.bin";
    std::fs::create_dir(fixture.root.path().join(directory)).unwrap();
    let server = fixture.start_uploads(64).await;
    let base = format!("http://{}", server.endpoints[0].address);
    let client = client();
    let location = format!(
        "/{}/{}",
        utf8_percent_encode(directory, NON_ALPHANUMERIC),
        utf8_percent_encode(filename, NON_ALPHANUMERIC)
    );
    let contents = b"\0binary\r\n\xffpayload";
    let response = client
        .put(format!(
            "{base}/{}/a%20%23%E4%BD%A0%E5%A5%BD.bin",
            utf8_percent_encode(directory, NON_ALPHANUMERIC)
        ))
        .header(header::ORIGIN, &base)
        .body(contents.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()[header::LOCATION], location);
    let response = client
        .get(format!("{base}{location}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), contents);
    assert_eq!(
        std::fs::read(fixture.root.path().join(directory).join(filename)).unwrap(),
        contents
    );

    let response = client.put(format!("{base}/empty")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()[header::LOCATION], "/empty");
    assert!(
        std::fs::read(fixture.root.path().join("empty"))
            .unwrap()
            .is_empty()
    );

    for path in [
        location.as_str(),
        "/empty",
        "/%E5%AD%90%20%E7%9B%AE%E5%BD%95",
    ] {
        let response = client
            .put(format!("{base}{path}"))
            .body("replacement")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{path}");
    }
    assert_eq!(
        std::fs::read(fixture.root.path().join(directory).join(filename)).unwrap(),
        contents
    );
    assert!(
        std::fs::read(fixture.root.path().join("empty"))
            .unwrap()
            .is_empty()
    );
    assert!(fixture.root.path().join(directory).is_dir());
    assert_eq!(temporary_upload_count(fixture.root.path()), 0);
    assert_eq!(
        temporary_upload_count(&fixture.root.path().join(directory)),
        0
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn multipart_uploads_validate_the_whole_form_before_publication() {
    let fixture = Fixture::new();
    let directory = fixture.root.path().join("子 目录");
    std::fs::create_dir(&directory).unwrap();
    let server = fixture.start_uploads(8).await;
    let base = format!("http://{}", server.endpoints[0].address);
    let client = client();
    let url = format!("{base}/%E5%AD%90%20%E7%9B%AE%E5%BD%95");
    let response = client
        .post(&url)
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=filemux-test",
        )
        .body(multipart(&[("file", "你好.bin", b"12345678")]))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/%E5%AD%90%20%E7%9B%AE%E5%BD%95/"
    );
    assert_eq!(
        std::fs::read(directory.join("你好.bin")).unwrap(),
        b"12345678"
    );

    let mut invalid_utf8 = multipart(&[("file", "invalid.bin", b"x")]);
    let offset = invalid_utf8
        .windows(11)
        .position(|bytes| bytes == b"invalid.bin")
        .unwrap();
    invalid_utf8[offset] = 0xff;
    let invalid_bodies = [
        multipart(&[("file", "first.bin", b"one"), ("file", "second.bin", b"two")]),
        multipart(&[("other", "wrong.bin", b"x")]),
        multipart(&[("file", "../escape.bin", b"x")]),
        multipart(&[("file", ".filemux-private", b"x")]),
        multipart(&[]),
        b"--filemux-test\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nx\r\n--filemux-test--\r\n".to_vec(),
        invalid_utf8,
    ];
    for body in invalid_bodies {
        let response = client
            .post(format!("{url}/"))
            .header(
                header::CONTENT_TYPE,
                "multipart/form-data; boundary=filemux-test",
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(directory.join("你好.bin")).unwrap(),
            b"12345678"
        );
    }
    let response = client
        .post(&url)
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=filemux-test",
        )
        .body(multipart(&[("file", "too-large.bin", b"123456789")]))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    assert!(!fixture.root.path().join("escape.bin").exists());

    let response = client.post(&url).body("not a form").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn upload_limits_cover_declared_and_chunked_bodies_without_publishing_errors() {
    let fixture = Fixture::new();
    let server = fixture.start_uploads(8).await;
    let address = server.endpoints[0].address;
    let base = format!("http://{address}");
    let client = client();
    let response = client
        .put(format!("{base}/declared-large"))
        .body("123456789")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let response = client
        .put(format!("{base}/declared-limit"))
        .body("12345678")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read(fixture.root.path().join("declared-limit")).unwrap(),
        b"12345678"
    );

    for (path, chunks, expected) in [
        ("chunked-large", "4\r\n1234\r\n5\r\n56789\r\n0\r\n\r\n", 413),
        ("chunked-limit", "4\r\n1234\r\n4\r\n5678\r\n0\r\n\r\n", 201),
    ] {
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket
            .write_all(
                format!(
                    "PUT /{path} HTTP/1.1\r\nHost: {address}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{chunks}"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        assert_eq!(read_status(socket).await, expected, "{path}");
    }
    assert_eq!(
        std::fs::read(fixture.root.path().join("chunked-limit")).unwrap(),
        b"12345678"
    );
    assert!(!fixture.root.path().join("declared-large").exists());
    assert!(!fixture.root.path().join("chunked-large").exists());
    assert_eq!(temporary_upload_count(fixture.root.path()), 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn upload_requests_enforce_paths_origins_and_supported_methods() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root.path().join("directory")).unwrap();
    let server = fixture.start_uploads(64).await;
    let base = format!("http://{}", server.endpoints[0].address);
    let client = client();
    for (path, expected) in [
        ("/", StatusCode::BAD_REQUEST),
        ("/directory/", StatusCode::BAD_REQUEST),
        ("/%2e%2e%2foutside", StatusCode::FORBIDDEN),
        ("/%2Efilemux-private", StatusCode::FORBIDDEN),
        ("/missing/child", StatusCode::NOT_FOUND),
    ] {
        let response = client
            .put(format!("{base}{path}"))
            .body("refused")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}");
    }
    let response = client
        .put(format!("{base}/range"))
        .header(header::CONTENT_RANGE, "bytes 0-2/3")
        .body("abc")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    for method in [reqwest::Method::PUT, reqwest::Method::POST] {
        for (name, value) in [
            (header::ORIGIN, "https://other.example"),
            (
                header::HeaderName::from_static("sec-fetch-site"),
                "cross-site",
            ),
        ] {
            let response = client
                .request(method.clone(), format!("{base}/cross-site"))
                .header(name, value)
                .body("refused")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    let response = client
        .delete(format!("{base}/unsupported"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()[header::ALLOW], "GET, HEAD, PUT, POST");
    assert_eq!(std::fs::read_dir(fixture.root.path()).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_dir(fixture.root.path().join("directory"))
            .unwrap()
            .count(),
        0
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupted_and_shutdown_uploads_remove_temporary_files_without_publication() {
    let fixture = Fixture::new();
    let server = fixture.start_uploads(64).await;
    let address = server.endpoints[0].address;
    let mut disconnected = TcpStream::connect(address).await.unwrap();
    disconnected
        .write_all(
            format!(
                "PUT /disconnected HTTP/1.1\r\nHost: {address}\r\nContent-Length: 16\r\n\r\npart"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    wait_for_upload_count(fixture.root.path(), 1).await;
    assert!(!fixture.root.path().join("disconnected").exists());
    drop(disconnected);
    wait_for_upload_count(fixture.root.path(), 0).await;
    assert!(!fixture.root.path().join("disconnected").exists());

    let mut interrupted = TcpStream::connect(address).await.unwrap();
    interrupted
        .write_all(
            format!("PUT /shutdown HTTP/1.1\r\nHost: {address}\r\nTransfer-Encoding: chunked\r\n\r\n4\r\npart\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    wait_for_upload_count(fixture.root.path(), 1).await;
    assert!(!fixture.root.path().join("shutdown").exists());
    tokio::time::timeout(Duration::from_secs(5), server.shutdown())
        .await
        .unwrap()
        .unwrap();
    wait_for_upload_count(fixture.root.path(), 0).await;
    assert_eq!(std::fs::read_dir(fixture.root.path()).unwrap().count(), 0);
    let mut byte = [0_u8; 1];
    let result = tokio::time::timeout(Duration::from_secs(2), interrupted.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
}

#[tokio::test]
async fn concurrent_uploads_to_one_name_publish_exactly_one_complete_body() {
    let fixture = Fixture::new();
    let server = fixture.start_uploads(8).await;
    let address = server.endpoints[0].address;
    let mut first = TcpStream::connect(address).await.unwrap();
    let mut second = TcpStream::connect(address).await.unwrap();
    let headers = format!(
        "PUT /race HTTP/1.1\r\nHost: {address}\r\nContent-Length: 8\r\nConnection: close\r\n\r\n"
    );
    first.write_all(headers.as_bytes()).await.unwrap();
    first.write_all(b"left").await.unwrap();
    second.write_all(headers.as_bytes()).await.unwrap();
    second.write_all(b"rght").await.unwrap();
    wait_for_upload_count(fixture.root.path(), 2).await;
    assert!(!fixture.root.path().join("race").exists());
    let (first_status, second_status) = tokio::join!(
        async move {
            first.write_all(b"LEFT").await.unwrap();
            read_status(first).await
        },
        async move {
            second.write_all(b"RGHT").await.unwrap();
            read_status(second).await
        }
    );
    assert!(
        matches!((first_status, second_status), (201, 409) | (409, 201)),
        "{first_status}, {second_status}"
    );
    let expected = if first_status == 201 {
        b"leftLEFT"
    } else {
        b"rghtRGHT"
    };
    assert_eq!(
        std::fs::read(fixture.root.path().join("race")).unwrap(),
        expected
    );
    assert_eq!(temporary_upload_count(fixture.root.path()), 0);
    server.shutdown().await.unwrap();
}
