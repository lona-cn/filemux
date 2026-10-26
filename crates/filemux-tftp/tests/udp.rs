use filemux_core::{ServerHandle, TftpConfig};
use filemux_fs::SharedFs;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::UdpSocket;
use tokio::time::timeout;

struct Fixture {
    _directory: TempDir,
    root: std::path::PathBuf,
    fs: Arc<SharedFs>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let fs = Arc::new(SharedFs::open(&root, &directory.path().join("state")).unwrap());
        Self {
            _directory: directory,
            root,
            fs,
        }
    }

    async fn start(&self, uploads: bool) -> ServerHandle {
        self.start_with(TftpConfig {
            allow_upload: uploads,
            ..test_config()
        })
        .await
    }

    async fn start_with(&self, config: TftpConfig) -> ServerHandle {
        let (events, receiver) = tokio::sync::broadcast::channel(16);
        // Absence of event receivers must never prevent a transfer.
        drop(receiver);
        filemux_tftp::start(self.fs.clone(), config, events)
            .await
            .unwrap()
    }
}

fn test_config() -> TftpConfig {
    TftpConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        timeout_ms: 100,
        retries: 2,
        ..TftpConfig::default()
    }
}

fn request(opcode: u16, name: &str, mode: &str, options: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = opcode.to_be_bytes().to_vec();
    for text in [name, mode] {
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
    }
    for (key, value) in options {
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    bytes
}

fn packet(opcode: u16, block: u16, data: &[u8]) -> Vec<u8> {
    let mut bytes = opcode.to_be_bytes().to_vec();
    bytes.extend_from_slice(&block.to_be_bytes());
    bytes.extend_from_slice(data);
    bytes
}

async fn client() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").await.unwrap()
}

async fn receive(socket: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    let mut bytes = vec![0; 65_536];
    let (length, source) = timeout(Duration::from_secs(3), socket.recv_from(&mut bytes))
        .await
        .expect("TFTP response timed out")
        .unwrap();
    bytes.truncate(length);
    (bytes, source)
}

async fn assert_silent(socket: &UdpSocket) {
    let mut bytes = [0; 65_536];
    assert!(
        timeout(Duration::from_millis(150), socket.recv_from(&mut bytes))
            .await
            .is_err(),
        "ERROR packets must not elicit a response"
    );
}

async fn begin(socket: &UdpSocket, server: &ServerHandle, bytes: &[u8]) -> (Vec<u8>, SocketAddr) {
    socket
        .send_to(bytes, server.endpoints[0].address)
        .await
        .unwrap();
    receive(socket).await
}

fn error_code(bytes: &[u8]) -> u16 {
    assert!(bytes.len() >= 5);
    assert_eq!(&bytes[..2], &[0, 5]);
    u16::from_be_bytes([bytes[2], bytes[3]])
}

async fn download(
    socket: &UdpSocket,
    server: &ServerHandle,
    name: &str,
    block_size: usize,
) -> Vec<u8> {
    let size = block_size.to_string();
    let options = if block_size == 512 {
        vec![]
    } else {
        vec![("blksize", size.as_str())]
    };
    let (mut data, tid) = begin(socket, server, &request(1, name, "octet", &options)).await;
    assert_ne!(
        tid.port(),
        server.endpoints[0].address.port(),
        "DATA must come from transfer TID"
    );
    if !options.is_empty() {
        assert_eq!(&data[..2], &[0, 6]);
        socket.send_to(&packet(4, 0, &[]), tid).await.unwrap();
        let (next, source) = receive(socket).await;
        assert_eq!(source, tid);
        data = next;
    }
    let mut result = Vec::new();
    let mut block = 1u16;
    loop {
        assert_eq!(&data[..4], &packet(3, block, &[]));
        let length = data.len() - 4;
        result.extend_from_slice(&data[4..]);
        socket.send_to(&packet(4, block, &[]), tid).await.unwrap();
        if length < block_size {
            return result;
        }
        let (next, source) = receive(socket).await;
        assert_eq!(source, tid);
        data = next;
        block = block.wrapping_add(1);
    }
}

#[tokio::test]
async fn downloads_empty_and_exact_block_files_with_empty_terminal_data() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("empty"), []).unwrap();
    let content: Vec<u8> = (0..1024).map(|index| (index % 251) as u8).collect();
    std::fs::write(fixture.root.join("exact"), &content).unwrap();
    let server = fixture.start(false).await;
    assert_ne!(server.endpoints[0].address.port(), 0);
    assert_eq!(
        download(&client().await, &server, "empty", 512).await,
        Vec::<u8>::new()
    );
    assert_eq!(
        download(&client().await, &server, "exact", 512).await,
        content
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn default_upload_denial_and_explicit_mode_and_path_rejection() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("file"), b"content").unwrap();
    let server = fixture.start(false).await;
    for (bytes, expected) in [
        (request(2, "new", "octet", &[]), 2),
        (request(1, "file", "netascii", &[]), 4),
        (request(1, "../outside", "octet", &[]), 2),
        (request(1, "C:/outside", "octet", &[]), 2),
        (request(1, "folder/.FILEMUX-secret", "octet", &[]), 2),
    ] {
        let (response, _) = begin(&client().await, &server, &bytes).await;
        assert_eq!(error_code(&response), expected);
    }
    assert!(!fixture.root.join("new").exists());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_request_is_rejected_without_stopping_the_listener() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("file"), b"still available").unwrap();
    let server = fixture.start(false).await;
    let socket = client().await;
    let oversized = request(1, "file", "octet", &[("ignored", &"x".repeat(60_000))]);
    let (response, source) = begin(&socket, &server, &oversized).await;
    assert_eq!(source, server.endpoints[0].address);
    assert_eq!(error_code(&response), 4);
    assert_eq!(
        download(&socket, &server, "file", 512).await,
        b"still available"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn listener_ignores_error_packets_and_remains_available() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("file"), b"still available").unwrap();
    let server = fixture.start(false).await;
    let socket = client().await;
    for error in [packet(5, 0, b"client aborted\0"), vec![0, 5]] {
        socket
            .send_to(&error, server.endpoints[0].address)
            .await
            .unwrap();
        assert_silent(&socket).await;
    }
    assert_eq!(
        download(&socket, &server, "file", 512).await,
        b"still available"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_tid_error_is_silent_and_does_not_abort_the_download() {
    let fixture = Fixture::new();
    let content = vec![0x5a; 513];
    std::fs::write(fixture.root.join("file"), &content).unwrap();
    let server = fixture
        .start_with(TftpConfig {
            timeout_ms: 1_000,
            ..test_config()
        })
        .await;
    let socket = client().await;
    let (data, tid) = begin(&socket, &server, &request(1, "file", "octet", &[])).await;
    assert_eq!(data, packet(3, 1, &content[..512]));
    let intruder = client().await;
    intruder
        .send_to(&packet(5, 0, b"not the transfer peer\0"), tid)
        .await
        .unwrap();
    assert_silent(&intruder).await;
    // Other wrong-TID packets still receive the standard error.
    intruder.send_to(&packet(4, 1, &[]), tid).await.unwrap();
    let (error, source) = receive(&intruder).await;
    assert_eq!(source, tid);
    assert_eq!(error_code(&error), 5);
    socket.send_to(&packet(4, 1, &[]), tid).await.unwrap();
    let (data, source) = receive(&socket).await;
    assert_eq!(source, tid);
    assert_eq!(data, packet(3, 2, &content[512..]));
    socket.send_to(&packet(4, 2, &[]), tid).await.unwrap();
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn peer_error_silently_aborts_upload_and_removes_private_data() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("file"), b"still available").unwrap();
    let (events, mut receiver) = tokio::sync::broadcast::channel(16);
    let server = filemux_tftp::start(
        fixture.fs.clone(),
        TftpConfig {
            allow_upload: true,
            timeout_ms: 1_000,
            ..test_config()
        },
        events,
    )
    .await
    .unwrap();
    let socket = client().await;
    let (ack, tid) = begin(&socket, &server, &request(2, "aborted", "octet", &[])).await;
    assert_eq!(ack, packet(4, 0, &[]));
    socket
        .send_to(&packet(3, 1, &[0xab; 512]), tid)
        .await
        .unwrap();
    assert_eq!(receive(&socket).await.0, packet(4, 1, &[]));
    socket
        .send_to(&packet(5, 0, b"client aborted\0"), tid)
        .await
        .unwrap();
    // The failure event follows writer closure and temporary-file cleanup.
    timeout(Duration::from_secs(3), async {
        loop {
            let event = receiver.recv().await.unwrap();
            if event.is_error {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_silent(&socket).await;
    assert!(!fixture.root.join("aborted").exists());
    assert_eq!(std::fs::read_dir(&fixture.root).unwrap().count(), 1);
    assert_eq!(
        download(&client().await, &server, "file", 512).await,
        b"still available"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn uploads_publish_before_final_ack_and_reack_duplicates_without_overwriting() {
    let fixture = Fixture::new();
    let server = fixture.start(true).await;
    let socket = client().await;
    let (ack, tid) = begin(&socket, &server, &request(2, "uploaded", "octet", &[])).await;
    assert_eq!(ack, packet(4, 0, &[]));
    assert_ne!(tid.port(), server.endpoints[0].address.port());
    let first = vec![0xab; 512];
    socket.send_to(&packet(3, 1, &first), tid).await.unwrap();
    assert_eq!(receive(&socket).await.0, packet(4, 1, &[]));
    assert!(
        !fixture.root.join("uploaded").exists(),
        "partial uploads must stay private"
    );
    socket.send_to(&packet(3, 1, &first), tid).await.unwrap();
    assert_eq!(receive(&socket).await.0, packet(4, 1, &[]));
    let final_packet = packet(3, 2, b"tail");
    socket.send_to(&final_packet, tid).await.unwrap();
    assert_eq!(receive(&socket).await.0, packet(4, 2, &[]));
    let mut expected = first;
    expected.extend_from_slice(b"tail");
    assert_eq!(
        std::fs::read(fixture.root.join("uploaded")).unwrap(),
        expected,
        "final ACK is success authority only after publication"
    );
    // Simulate loss of the final ACK: the completed TID must dally and ACK again.
    socket.send_to(&final_packet, tid).await.unwrap();
    assert_eq!(receive(&socket).await.0, packet(4, 2, &[]));
    let other = client().await;
    let (response, existing_tid) =
        begin(&other, &server, &request(2, "uploaded", "octet", &[])).await;
    // Filesystems may reject at begin_upload or atomically at commit. Neither
    // case may report success or modify the existing bytes.
    if response == packet(4, 0, &[]) {
        other
            .send_to(&packet(3, 1, b"replacement"), existing_tid)
            .await
            .unwrap();
        assert_eq!(error_code(&receive(&other).await.0), 6);
    } else {
        assert_eq!(error_code(&response), 6);
    }
    assert_eq!(
        std::fs::read(fixture.root.join("uploaded")).unwrap(),
        expected
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn option_negotiation_sizes_and_exact_upload_terminal_block() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("file"), b"0123456789abcdef").unwrap();
    let server = fixture.start(true).await;
    let socket = client().await;
    let options = [
        ("blksize", "8"),
        ("timeout", "1"),
        ("tsize", "0"),
        ("windowsize", "4"),
    ];
    let (oack, tid) = begin(&socket, &server, &request(1, "file", "OCTET", &options)).await;
    assert_eq!(oack, b"\0\x06blksize\x008\0timeout\x001\0tsize\x0016\0");
    socket.send_to(&packet(4, 0, &[]), tid).await.unwrap();
    for (block, expected) in [(1, &b"01234567"[..]), (2, &b"89abcdef"[..]), (3, &b""[..])] {
        let (data, source) = receive(&socket).await;
        assert_eq!(source, tid);
        assert_eq!(data, packet(3, block, expected));
        socket.send_to(&packet(4, block, &[]), tid).await.unwrap();
    }
    let socket = client().await;
    let options = [("blksize", "8"), ("tsize", "8")];
    let (oack, tid) = begin(&socket, &server, &request(2, "exact", "octet", &options)).await;
    assert_eq!(oack, b"\0\x06blksize\x008\0tsize\x008\0");
    socket
        .send_to(&packet(3, 1, b"12345678"), tid)
        .await
        .unwrap();
    assert_eq!(receive(&socket).await.0, packet(4, 1, &[]));
    assert!(!fixture.root.join("exact").exists());
    socket.send_to(&packet(3, 2, b""), tid).await.unwrap();
    assert_eq!(receive(&socket).await.0, packet(4, 2, &[]));
    assert_eq!(
        std::fs::read(fixture.root.join("exact")).unwrap(),
        b"12345678"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn transfer_tid_retransmission_and_limits_are_enforced_and_shutdown_cleans_uploads() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("file"), b"data").unwrap();
    let server = fixture
        .start_with(TftpConfig {
            max_transfers: 1,
            allow_upload: true,
            max_upload_bytes: 8,
            ..test_config()
        })
        .await;
    let socket = client().await;
    let (data, tid) = begin(&socket, &server, &request(1, "file", "octet", &[])).await;
    let (retry, retry_tid) = receive(&socket).await;
    assert_eq!(retry, data);
    assert_eq!(retry_tid, tid);
    let intruder = client().await;
    intruder.send_to(&packet(4, 1, &[]), tid).await.unwrap();
    assert_eq!(error_code(&receive(&intruder).await.0), 5);
    let (busy, _) = begin(&intruder, &server, &request(1, "file", "octet", &[])).await;
    assert_eq!(error_code(&busy), 0);
    // Use a fresh server to avoid races with the previous task's slot release.
    server.shutdown().await.unwrap();
    let server = fixture
        .start_with(TftpConfig {
            allow_upload: true,
            max_upload_bytes: 8,
            ..test_config()
        })
        .await;
    let socket = client().await;
    let (ack, tid) = begin(&socket, &server, &request(2, "large", "octet", &[])).await;
    assert_eq!(ack, packet(4, 0, &[]));
    socket
        .send_to(&packet(3, 1, b"123456789"), tid)
        .await
        .unwrap();
    assert_eq!(error_code(&receive(&socket).await.0), 3);
    assert!(!fixture.root.join("large").exists());
    let pending = client().await;
    let (ack, _) = begin(&pending, &server, &request(2, "cancelled", "octet", &[])).await;
    assert_eq!(ack, packet(4, 0, &[]));
    server.shutdown().await.unwrap();
    assert!(!fixture.root.join("cancelled").exists());
    let names: Vec<_> = std::fs::read_dir(&fixture.root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        vec![std::ffi::OsString::from("file")],
        "aborts must remove private temporary files"
    );
}

#[tokio::test]
async fn block_numbers_wrap_through_zero_on_a_real_download() {
    let fixture = Fixture::new();
    let content: Vec<u8> = (0..8 * 65_535).map(|index| (index % 251) as u8).collect();
    std::fs::write(fixture.root.join("rollover"), &content).unwrap();
    let server = fixture.start(false).await;
    let socket = client().await;
    assert_eq!(download(&socket, &server, "rollover", 8).await, content);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn publication_race_never_sends_success_ack_or_overwrites_the_winner() {
    let fixture = Fixture::new();
    let server = fixture.start(true).await;
    let socket = client().await;
    let (ack, tid) = begin(&socket, &server, &request(2, "race", "octet", &[])).await;
    assert_eq!(ack, packet(4, 0, &[]));
    std::fs::write(fixture.root.join("race"), b"existing winner").unwrap();
    socket
        .send_to(&packet(3, 1, b"must not replace"), tid)
        .await
        .unwrap();
    assert_eq!(error_code(&receive(&socket).await.0), 6);
    assert_eq!(
        std::fs::read(fixture.root.join("race")).unwrap(),
        b"existing winner"
    );
    let empty = client().await;
    let (ack, tid) = begin(&empty, &server, &request(2, "empty", "octet", &[])).await;
    assert_eq!(ack, packet(4, 0, &[]));
    empty.send_to(&packet(3, 1, b""), tid).await.unwrap();
    assert_eq!(receive(&empty).await.0, packet(4, 1, &[]));
    assert_eq!(std::fs::read(fixture.root.join("empty")).unwrap(), b"");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn abandoned_uploads_exhaust_a_finite_retry_budget_and_bad_options_are_rejected() {
    let fixture = Fixture::new();
    let server = fixture.start(true).await;
    for options in [
        vec![("blksize", "7")],
        vec![("timeout", "0")],
        vec![("tsize", "not-a-number")],
        vec![("blksize", "8"), ("BLKSIZE", "16")],
    ] {
        let (response, _) = begin(
            &client().await,
            &server,
            &request(2, "invalid", "octet", &options),
        )
        .await;
        assert_eq!(error_code(&response), 8);
    }
    let socket = client().await;
    let (ack, tid) = begin(&socket, &server, &request(2, "abandoned", "octet", &[])).await;
    assert_eq!(ack, packet(4, 0, &[]));
    // Exactly the configured retries, then a terminal error; not an unbounded
    // receive loop that leaves a transfer slot and private file behind.
    for _ in 0..test_config().retries {
        let (retry, source) = receive(&socket).await;
        assert_eq!(source, tid);
        assert_eq!(retry, ack);
    }
    assert_eq!(error_code(&receive(&socket).await.0), 0);
    assert!(!fixture.root.join("abandoned").exists());
    server.shutdown().await.unwrap();
    assert_eq!(std::fs::read_dir(&fixture.root).unwrap().count(), 0);
}
