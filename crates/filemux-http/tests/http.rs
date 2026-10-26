use filemux_core::{HttpConfig, ServerHandle};
use filemux_fs::SharedFs;
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
        let (events, _) = tokio::sync::broadcast::channel(32);
        filemux_http::start(
            self.fs.clone(),
            HttpConfig {
                enabled: false,
                bind: "127.0.0.1:0".parse().unwrap(),
                max_connections,
            },
            events,
        )
        .await
        .unwrap()
    }
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
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
