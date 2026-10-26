//! Independent client packets: no server codec or internal dispatcher is used here.
use filemux_core::fs::SharedFs;
use filemux_core::{NfsConfig, ServerHandle, Transport, nfs};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

fn word(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn hyper(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn opaque(out: &mut Vec<u8>, value: &[u8]) {
    word(out, value.len() as u32);
    out.extend_from_slice(value);
    out.resize(out.len() + (4 - value.len() % 4) % 4, 0);
}

fn call(program: u32, version: u32, procedure: u32, args: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in [0x10203040, 0, 2, program, version, procedure, 0, 0, 0, 0] {
        word(&mut out, value);
    }
    out.extend_from_slice(args);
    out
}

struct Decode<'a>(&'a [u8]);
impl<'a> Decode<'a> {
    fn bytes(&mut self, length: usize) -> &'a [u8] {
        let (value, rest) = self.0.split_at(length);
        self.0 = rest;
        value
    }
    fn word(&mut self) -> u32 {
        u32::from_be_bytes(self.bytes(4).try_into().unwrap())
    }
    fn hyper(&mut self) -> u64 {
        u64::from_be_bytes(self.bytes(8).try_into().unwrap())
    }
    fn opaque(&mut self) -> &'a [u8] {
        let length = self.word() as usize;
        let bytes = self.bytes(length);
        self.bytes((4 - length % 4) % 4);
        bytes
    }
    fn post(&mut self) {
        match self.word() {
            0 => {}
            1 => {
                self.bytes(84);
            }
            other => panic!("invalid post_op_attr discriminator {other}"),
        }
    }
    fn wcc(&mut self) {
        match self.word() {
            0 => {}
            1 => {
                self.bytes(24);
            }
            other => panic!("invalid pre_op_attr discriminator {other}"),
        }
        self.post();
    }
    fn end(&self) {
        assert!(self.0.is_empty(), "trailing {} bytes", self.0.len());
    }
}

async fn exchange(address: SocketAddr, transport: Transport, packet: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(5), async {
        match transport {
            Transport::Udp => {
                let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                socket.connect(address).await.unwrap();
                socket.send(packet).await.unwrap();
                let mut bytes = vec![0; 65536];
                let length = socket.recv(&mut bytes).await.unwrap();
                bytes.truncate(length);
                bytes
            }
            Transport::Tcp => {
                let mut socket = TcpStream::connect(address).await.unwrap();
                // Two RPC record fragments exercise framing, not just the single-fragment case.
                let split = packet.len().min(13);
                socket.write_u32(split as u32).await.unwrap();
                socket.write_all(&packet[..split]).await.unwrap();
                socket
                    .write_u32(0x80000000 | (packet.len() - split) as u32)
                    .await
                    .unwrap();
                socket.write_all(&packet[split..]).await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let marker = socket.read_u32().await.unwrap();
                    let start = bytes.len();
                    bytes.resize(start + (marker & 0x7fffffff) as usize, 0);
                    socket.read_exact(&mut bytes[start..]).await.unwrap();
                    if marker & 0x80000000 != 0 {
                        break;
                    }
                }
                bytes
            }
        }
    })
    .await
    .expect("RPC exchange timed out")
}

fn body(bytes: &[u8]) -> Decode<'_> {
    let mut input = Decode(bytes);
    assert_eq!(input.word(), 0x10203040);
    assert_eq!(input.word(), 1); // REPLY
    assert_eq!(input.word(), 0); // MSG_ACCEPTED
    assert_eq!(input.word(), 0); // AUTH_NONE verifier
    assert!(input.opaque().is_empty());
    assert_eq!(input.word(), 0); // SUCCESS
    input
}

async fn invoke(
    address: SocketAddr,
    transport: Transport,
    program: u32,
    procedure: u32,
    args: &[u8],
) -> Vec<u8> {
    exchange(address, transport, &call(program, 3, procedure, args)).await
}

fn endpoint(server: &ServerHandle, role: &str, transport: Transport) -> SocketAddr {
    server
        .endpoints
        .iter()
        .find(|value| value.role == role && value.transport == transport)
        .unwrap()
        .address
}

async fn fixture() -> (tempfile::TempDir, Arc<SharedFs>, ServerHandle) {
    fixture_with_export("/").await
}

async fn fixture_with_export(export: &str) -> (tempfile::TempDir, Arc<SharedFs>, ServerHandle) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("share");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("dir")).unwrap();
    std::fs::write(root.join("hello"), b"hello NFS\n").unwrap();
    std::fs::write(root.join("dir").join("nested"), b"nested bytes").unwrap();
    let fs = Arc::new(SharedFs::open(&root, &temp.path().join("state")).unwrap());
    let config = NfsConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        rpcbind: Some("127.0.0.1:0".parse().unwrap()),
        export: export.to_owned(),
        ..NfsConfig::default()
    };
    let (events, _) = tokio::sync::broadcast::channel(8);
    let server = nfs::start(fs.clone(), config, events).await.unwrap();
    (temp, fs, server)
}

async fn mount(address: SocketAddr, transport: Transport) -> Vec<u8> {
    mount_path(address, transport, b"/").await
}

async fn mount_path(address: SocketAddr, transport: Transport, path: &[u8]) -> Vec<u8> {
    let mut args = Vec::new();
    opaque(&mut args, path);
    let reply = invoke(address, transport, 100005, 1, &args).await;
    let mut input = body(&reply);
    assert_eq!(input.word(), 0);
    let handle = input.opaque().to_vec();
    assert!(handle.len() <= 64);
    assert_eq!(input.word(), 2);
    assert_eq!(input.word(), 0);
    assert_eq!(input.word(), 1);
    input.end();
    handle
}

async fn lookup(address: SocketAddr, transport: Transport, dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut args = Vec::new();
    opaque(&mut args, dir);
    opaque(&mut args, name);
    let reply = invoke(address, transport, 100003, 3, &args).await;
    let mut input = body(&reply);
    assert_eq!(input.word(), 0);
    let handle = input.opaque().to_vec();
    input.post();
    input.post();
    input.end();
    handle
}

async fn read(address: SocketAddr, transport: Transport, handle: &[u8], expected: &[u8]) {
    let mut args = Vec::new();
    opaque(&mut args, handle);
    hyper(&mut args, 0);
    word(&mut args, 4096);
    let reply = invoke(address, transport, 100003, 6, &args).await;
    let mut input = body(&reply);
    assert_eq!(input.word(), 0);
    input.post();
    assert_eq!(input.word(), expected.len() as u32);
    assert_eq!(input.word(), 1);
    assert_eq!(input.opaque(), expected);
    input.end();
}

#[tokio::test]
async fn subdirectory_mount_returns_the_directory_handle_and_rejects_escapes() {
    let (temp, _fs, server) = fixture().await;
    #[cfg(unix)]
    let _socket = {
        let root = temp.path().join("share");
        std::os::unix::fs::symlink("dir", root.join("dir-link")).unwrap();
        std::os::unix::fs::symlink(temp.path(), root.join("outside-link")).unwrap();
        std::os::unix::net::UnixListener::bind(root.join("socket")).unwrap()
    };
    #[cfg(not(unix))]
    let _ = &temp;
    for transport in [Transport::Udp, Transport::Tcp] {
        let address = endpoint(&server, "nfs", transport);
        let root = mount(address, transport).await;
        let dir = mount_path(address, transport, b"/dir").await;
        assert_eq!(dir, lookup(address, transport, &root, b"dir").await);
        assert_ne!(dir, root);
        let nested = lookup(address, transport, &dir, b"nested").await;
        read(address, transport, &nested, b"nested bytes").await;
        let mut rejected: Vec<(&[u8], u32)> = vec![
            (b"/../dir", 13),
            (b"/dir/../dir", 13),
            (b"/dir/./", 13),
            (b"/dir\\..\\dir", 13),
            (b"/dir\0", 13),
            (b"/hello", 20),
            (b"/hello/child", 20),
            (b"/missing", 2),
        ];
        #[cfg(unix)]
        rejected.extend([
            (b"/dir-link".as_slice(), 20),
            (b"/dir-link/nested".as_slice(), 20),
            (b"/outside-link".as_slice(), 20),
            (b"/socket".as_slice(), 20),
        ]);
        for (path, status) in rejected.drain(..) {
            let mut args = Vec::new();
            opaque(&mut args, path);
            let reply = invoke(address, transport, 100005, 1, &args).await;
            let mut input = body(&reply);
            assert_eq!(input.word(), status, "MOUNT {path:?}");
            input.end();
        }
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn custom_export_mount_obeys_component_boundaries() {
    let (_temp, _fs, server) = fixture_with_export("/boot").await;
    for transport in [Transport::Udp, Transport::Tcp] {
        let address = endpoint(&server, "nfs", transport);
        let root = mount_path(address, transport, b"/boot").await;
        let dir = mount_path(address, transport, b"/boot/dir").await;
        assert_eq!(dir, lookup(address, transport, &root, b"dir").await);
        let nested = lookup(address, transport, &dir, b"nested").await;
        read(address, transport, &nested, b"nested bytes").await;
        for (path, status) in [
            (b"/bootleg/dir".as_slice(), 2),
            (b"/boots".as_slice(), 2),
            (b"/dir".as_slice(), 2),
            (b"/".as_slice(), 2),
            (b"/boot/../dir".as_slice(), 13),
            (b"/boot/dir/../../hello".as_slice(), 13),
            (b"/boot/hello".as_slice(), 20),
        ] {
            let mut args = Vec::new();
            opaque(&mut args, path);
            let reply = invoke(address, transport, 100005, 1, &args).await;
            let mut input = body(&reply);
            assert_eq!(input.word(), status, "MOUNT {path:?}");
            input.end();
        }
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn uboot_read_accepts_only_one_optional_zero_word() {
    let (_temp, _fs, server) = fixture().await;
    for transport in [Transport::Udp, Transport::Tcp] {
        let address = endpoint(&server, "nfs", transport);
        let root = mount(address, transport).await;
        let file = lookup(address, transport, &root, b"hello").await;
        // rpc_add_credentials + nfs_read_req from U-Boot net/nfs-common.c:
        // AUTH_SYS has an empty hostname, zero stamp/uid/gid/groups; READ v3
        // contains the file handle, 64-bit offset, count and one extra zero.
        let mut packet = Vec::new();
        for value in [0x10203040, 0, 2, 100003, 3, 6, 1, 20, 0, 0, 0, 0, 0, 0, 0] {
            word(&mut packet, value);
        }
        opaque(&mut packet, &file);
        hyper(&mut packet, 2);
        word(&mut packet, 4);
        let standard_length = packet.len();
        word(&mut packet, 0);
        let reply = exchange(address, transport, &packet).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 0);
        input.post();
        assert_eq!(input.word(), 4);
        assert_eq!(input.word(), 0); // not EOF
        assert_eq!(input.opaque(), b"llo ");
        input.end();

        packet.truncate(standard_length);
        for trailing in [
            &[0, 0, 0, 1][..], // one word, but not zero
            &[0; 8][..],       // two zero words
            &[0][..],          // incomplete word
            &[0; 5][..],       // zero word plus garbage
        ] {
            let mut invalid = packet.clone();
            invalid.extend_from_slice(trailing);
            let reply = exchange(address, transport, &invalid).await;
            let mut input = Decode(&reply);
            for expected in [0x10203040, 1, 0, 0, 0, 4] {
                assert_eq!(input.word(), expected); // RPC GARBAGE_ARGS
            }
            input.end();
        }
        let mut getattr = Vec::new();
        opaque(&mut getattr, &file);
        word(&mut getattr, 0);
        let mut mount = Vec::new();
        opaque(&mut mount, b"/dir");
        word(&mut mount, 0);
        for packet in [call(100003, 3, 1, &getattr), call(100005, 3, 1, &mount)] {
            let reply = exchange(address, transport, &packet).await;
            let mut input = Decode(&reply);
            for expected in [0x10203040, 1, 0, 0, 0, 4] {
                assert_eq!(input.word(), expected); // other procedures remain strict
            }
            input.end();
        }
        read(address, transport, &file, b"hello NFS\n").await;
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn udp_and_tcp_mount_lookup_read_readdir_refuse_write() {
    let (_temp, _fs, server) = fixture().await;
    for transport in [Transport::Udp, Transport::Tcp] {
        let address = endpoint(&server, "nfs", transport);
        let root = mount(address, transport).await;
        let hello = lookup(address, transport, &root, b"hello").await;
        read(address, transport, &hello, b"hello NFS\n").await;
        let dir = lookup(address, transport, &root, b"dir").await;
        let nested = lookup(address, transport, &dir, b"nested").await;
        read(address, transport, &nested, b"nested bytes").await;
        assert_eq!(lookup(address, transport, &root, b"..").await, root);
        assert_eq!(lookup(address, transport, &dir, b"..").await, root);
        assert_eq!(lookup(address, transport, &dir, b".").await, dir);

        let mut args = Vec::new();
        opaque(&mut args, &root);
        hyper(&mut args, 0);
        hyper(&mut args, 0);
        word(&mut args, 4096);
        let reply = invoke(address, transport, 100003, 16, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 0);
        input.post();
        input.bytes(8);
        let mut names = Vec::new();
        while input.word() == 1 {
            input.hyper();
            names.push(input.opaque().to_vec());
            assert!(input.hyper() > 0);
        }
        assert_eq!(input.word(), 1); // eof
        input.end();
        names.sort();
        assert_eq!(names, [b"dir".to_vec(), b"hello".to_vec()]);

        let mut args = Vec::new();
        opaque(&mut args, &hello);
        hyper(&mut args, 0);
        word(&mut args, 3);
        word(&mut args, 2); // FILE_SYNC
        opaque(&mut args, b"bad");
        let reply = invoke(address, transport, 100003, 7, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 30); // NFS3ERR_ROFS
        input.wcc();
        input.end();
        read(address, transport, &hello, b"hello NFS\n").await;

        let mut args = Vec::new();
        opaque(&mut args, &root);
        opaque(&mut args, b"hello");
        let reply = invoke(address, transport, 100003, 12, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 30);
        input.wcc();
        input.end();
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn portmapper_reports_actual_ports_and_protocols() {
    let (_temp, _fs, server) = fixture().await;
    for transport in [Transport::Tcp, Transport::Udp] {
        let address = endpoint(&server, "portmapper", transport);
        for (program, version, protocol, expected) in [
            (
                100003,
                3,
                6,
                endpoint(&server, "nfs", Transport::Tcp).port(),
            ),
            (
                100003,
                3,
                17,
                endpoint(&server, "nfs", Transport::Udp).port(),
            ),
            (
                100005,
                3,
                6,
                endpoint(&server, "nfs", Transport::Tcp).port(),
            ),
            (
                100005,
                3,
                17,
                endpoint(&server, "nfs", Transport::Udp).port(),
            ),
            (100000, 2, 17, address.port()),
            (100003, 3, 99, 0),
            (100003, 2, 6, 0),
        ] {
            let mut args = Vec::new();
            for value in [program, version, protocol, 0] {
                word(&mut args, value);
            }
            let reply = exchange(address, transport, &call(100000, 2, 3, &args)).await;
            let mut input = body(&reply);
            assert_eq!(input.word(), u32::from(expected));
            input.end();
        }
        let reply = exchange(address, transport, &call(100000, 2, 4, &[])).await;
        let mut input = body(&reply);
        let mut mappings = Vec::new();
        while input.word() == 1 {
            mappings.push((input.word(), input.word(), input.word(), input.word()));
        }
        input.end();
        assert_eq!(mappings.len(), 6);
        assert!(mappings.contains(&(
            100003,
            3,
            6,
            u32::from(endpoint(&server, "nfs", transport).port())
        )));
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn directory_pagination_plus_verifier_and_small_buffer() {
    let (temp, _fs, server) = fixture().await;
    for index in 0..12 {
        std::fs::write(
            temp.path().join("share").join(format!("file-{index:02}")),
            b"x",
        )
        .unwrap();
    }
    for transport in [Transport::Tcp, Transport::Udp] {
        let address = endpoint(&server, "nfs", transport);
        let root = mount(address, transport).await;
        let mut cookie = 0;
        let mut verifier = [0_u8; 8];
        let mut names = Vec::new();
        for _ in 0..20 {
            let mut args = Vec::new();
            opaque(&mut args, &root);
            hyper(&mut args, cookie);
            args.extend_from_slice(&verifier);
            word(&mut args, 128); // dircount, separate from full reply maxcount
            word(&mut args, 512); // one or two full entries per page
            let reply = invoke(address, transport, 100003, 17, &args).await;
            let mut input = body(&reply);
            assert_eq!(input.word(), 0);
            input.post();
            verifier.copy_from_slice(input.bytes(8));
            let old_cookie = cookie;
            while input.word() == 1 {
                input.hyper();
                names.push(input.opaque().to_vec());
                cookie = input.hyper();
                input.post();
                assert_eq!(input.word(), 1);
                assert!(!input.opaque().is_empty());
            }
            let eof = input.word() == 1;
            input.end();
            assert!(cookie > old_cookie);
            if eof {
                break;
            }
        }
        assert_eq!(names.len(), 14);
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 14);

        let mut args = Vec::new();
        opaque(&mut args, &root);
        hyper(&mut args, cookie);
        args.extend_from_slice(&[0xff; 8]);
        word(&mut args, 4096);
        let reply = invoke(address, transport, 100003, 16, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 10003);
        input.post();
        input.end();

        let mut args = Vec::new();
        opaque(&mut args, &root);
        hyper(&mut args, 0);
        hyper(&mut args, 0);
        word(&mut args, 1);
        let reply = invoke(address, transport, 100003, 16, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 10005);
        input.post();
        input.end();
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn errors_authentication_and_handle_validation_are_wire_correct() {
    let (_temp, _fs, server) = fixture().await;
    for transport in [Transport::Tcp, Transport::Udp] {
        let address = endpoint(&server, "nfs", transport);
        for (packet, expected_status) in [
            (call(12345, 3, 0, &[]), 1),             // PROG_UNAVAIL
            (call(100003, 4, 0, &[]), 2),            // PROG_MISMATCH
            (call(100003, 3, 99, &[]), 3),           // PROC_UNAVAIL
            (call(100003, 3, 1, &[0, 0, 0, 64]), 4), // GARBAGE_ARGS, truncated fh
            (call(100003, 3, 0, &[0]), 4),           // extraneous args
        ] {
            let reply = exchange(address, transport, &packet).await;
            let mut input = Decode(&reply);
            for word in [0x10203040, 1, 0, 0, 0, expected_status] {
                assert_eq!(input.word(), word);
            }
            if expected_status == 2 {
                assert_eq!(input.word(), 3);
                assert_eq!(input.word(), 3);
            }
            input.end();
        }
        let mut packet = call(100003, 3, 0, &[]);
        packet[8..12].copy_from_slice(&3_u32.to_be_bytes()); // bad RPC version
        let reply = exchange(address, transport, &packet).await;
        let mut input = Decode(&reply);
        for word in [0x10203040, 1, 1, 0, 2, 2] {
            assert_eq!(input.word(), word);
        }
        input.end();

        let mut packet = call(100003, 3, 0, &[]);
        packet[24..28].copy_from_slice(&99_u32.to_be_bytes());
        let reply = exchange(address, transport, &packet).await;
        let mut input = Decode(&reply);
        for word in [0x10203040, 1, 1, 1, 2] {
            assert_eq!(input.word(), word);
        }
        input.end();

        let mut packet = Vec::new();
        for word_value in [0x10203040, 0, 2, 100003, 3, 0, 1] {
            word(&mut packet, word_value);
        }
        let mut sys = Vec::new();
        word(&mut sys, 1);
        opaque(&mut sys, b"loopback");
        for value in [1234, 1234, 1, 1234] {
            word(&mut sys, value);
        }
        opaque(&mut packet, &sys);
        word(&mut packet, 0);
        word(&mut packet, 0);
        body(&exchange(address, transport, &packet).await).end();

        let root = mount(address, transport).await;
        let mut wrong_export = root.clone();
        wrong_export[4] ^= 0xff;
        for (handle, expected) in [(wrong_export, 70), (vec![1, 2, 3], 10001)] {
            let mut args = Vec::new();
            opaque(&mut args, &handle);
            let reply = invoke(address, transport, 100003, 1, &args).await;
            let mut input = body(&reply);
            assert_eq!(input.word(), expected);
            input.end();
        }
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn metadata_queries_and_commit_validate_real_objects() {
    let (_temp, _fs, server) = fixture().await;
    for transport in [Transport::Tcp, Transport::Udp] {
        let address = endpoint(&server, "nfs", transport);
        let root = mount(address, transport).await;
        let hello = lookup(address, transport, &root, b"hello").await;
        let mut handle = Vec::new();
        opaque(&mut handle, &hello);
        let reply = invoke(address, transport, 100003, 1, &handle).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 0);
        assert_eq!(input.word(), 1); // NF3REG, not a directory/symlink impersonation
        input.bytes(16);
        assert_eq!(input.hyper(), 10);
        input.bytes(56);
        input.end();

        let mut args = handle.clone();
        word(&mut args, 63);
        let reply = invoke(address, transport, 100003, 4, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 0);
        input.post();
        let access = input.word();
        assert_eq!(access & (4 | 8 | 16), 0);
        assert_eq!(access & 1, 1);
        input.end();

        for procedure in [18, 19, 20] {
            let reply = invoke(address, transport, 100003, procedure, &handle).await;
            let mut input = body(&reply);
            assert_eq!(input.word(), 0);
            input.post();
            match procedure {
                18 => {
                    assert!(input.hyper() > 0);
                    input.hyper();
                    assert_eq!(input.hyper(), 0);
                    input.bytes(28);
                }
                19 => {
                    assert!(input.word() >= 32768);
                    input.bytes(24);
                    assert!(input.hyper() > 0);
                    input.bytes(12);
                }
                20 => {
                    assert_eq!(input.word(), 1);
                    assert_eq!(input.word(), 255);
                    input.bytes(16);
                }
                _ => unreachable!(),
            }
            input.end();
        }
        let mut args = handle;
        hyper(&mut args, 0);
        word(&mut args, 0);
        let reply = invoke(address, transport, 100003, 21, &args).await;
        let mut input = body(&reply);
        let status = input.word();
        assert!(
            status == 0 || status == 5,
            "host flush failure must be IO, not fabricated success/ROFS"
        );
        input.wcc();
        if status == 0 {
            input.bytes(8);
        }
        input.end();

        let mut args = Vec::new();
        opaque(&mut args, &root);
        hyper(&mut args, 0);
        word(&mut args, 0);
        let reply = invoke(address, transport, 100003, 21, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 5); // COMMIT directory must not be generic success
        input.wcc();
        input.end();
    }
    server.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn readlink_exposes_target_but_read_never_follows_it() {
    let (temp, _fs, server) = fixture().await;
    std::os::unix::fs::symlink("../private", temp.path().join("share/link")).unwrap();
    std::fs::write(temp.path().join("private"), b"not exported").unwrap();
    for transport in [Transport::Tcp, Transport::Udp] {
        let address = endpoint(&server, "nfs", transport);
        let root = mount(address, transport).await;
        let link = lookup(address, transport, &root, b"link").await;
        let mut args = Vec::new();
        opaque(&mut args, &link);
        let reply = invoke(address, transport, 100003, 5, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 0);
        input.post();
        assert_eq!(input.opaque(), b"../private");
        input.end();
        hyper(&mut args, 0);
        word(&mut args, 4096);
        let reply = invoke(address, transport, 100003, 6, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 22);
        input.post();
        input.end();
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_tcp_records_and_active_connections_are_closed() {
    let (_temp, _fs, server) = fixture().await;
    let address = endpoint(&server, "nfs", Transport::Tcp);
    let mut oversized = TcpStream::connect(address).await.unwrap();
    oversized
        .write_u32(0x80000000 | (4 * 1024 * 1024 + 1))
        .await
        .unwrap();
    let mut byte = [0];
    let result = tokio::time::timeout(Duration::from_secs(5), oversized.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    drop(oversized);
    let mut active = TcpStream::connect(address).await.unwrap();
    active.write_u32(100).await.unwrap(); // unfinished fragment must not delay shutdown
    let tcp = address;
    let udp = endpoint(&server, "nfs", Transport::Udp);
    let pmap = endpoint(&server, "portmapper", Transport::Tcp);
    tokio::time::timeout(Duration::from_secs(5), server.shutdown())
        .await
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), active.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    drop(active);
    let _tcp = tokio::net::TcpListener::bind(tcp).await.unwrap();
    let _udp = UdpSocket::bind(udp).await.unwrap();
    let _portmap = tokio::net::TcpListener::bind(pmap).await.unwrap();
}

#[tokio::test]
async fn every_modifying_procedure_returns_its_rofs_failure_union() {
    let (_temp, _fs, server) = fixture().await;
    for transport in [Transport::Tcp, Transport::Udp] {
        let address = endpoint(&server, "nfs", transport);
        let root = mount(address, transport).await;
        let hello = lookup(address, transport, &root, b"hello").await;
        for procedure in [2, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
            let mut args = Vec::new();
            opaque(
                &mut args,
                if matches!(procedure, 2 | 7 | 15) {
                    &hello
                } else {
                    &root
                },
            );
            match procedure {
                2 => {
                    for _ in 0..7 {
                        word(&mut args, 0);
                    } // sattr3 + absent guard
                }
                7 => {
                    hyper(&mut args, 0);
                    word(&mut args, 1);
                    word(&mut args, 0); // UNSTABLE is refused too
                    opaque(&mut args, b"x");
                }
                8..=11 => {
                    opaque(&mut args, b"new");
                    if procedure == 8 {
                        word(&mut args, 0);
                    } // UNCHECKED
                    if procedure == 11 {
                        word(&mut args, 3);
                    } // NF3BLK
                    for _ in 0..6 {
                        word(&mut args, 0);
                    } // sattr3
                    if procedure == 10 {
                        opaque(&mut args, b"hello");
                    }
                    if procedure == 11 {
                        word(&mut args, 1);
                        word(&mut args, 3);
                    }
                }
                12 | 13 => opaque(&mut args, b"hello"),
                14 => {
                    opaque(&mut args, b"hello");
                    opaque(&mut args, &root);
                    opaque(&mut args, b"renamed");
                }
                15 => {
                    opaque(&mut args, &root);
                    opaque(&mut args, b"linked");
                }
                _ => unreachable!(),
            }
            let reply = invoke(address, transport, 100003, procedure, &args).await;
            let mut input = body(&reply);
            assert_eq!(input.word(), 30, "procedure {procedure}");
            if procedure == 15 {
                input.post();
            }
            input.wcc();
            if procedure == 14 {
                input.wcc();
            }
            input.end();
        }
        read(address, transport, &hello, b"hello NFS\n").await;
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn old_file_and_directory_handles_survive_reopening_persistent_state() {
    let (temp, fs, server) = fixture().await;
    let address = endpoint(&server, "nfs", Transport::Udp);
    let root = mount(address, Transport::Udp).await;
    let dir = mount_path(address, Transport::Udp, b"/dir").await;
    assert_eq!(dir, lookup(address, Transport::Udp, &root, b"dir").await);
    let file = lookup(address, Transport::Udp, &dir, b"nested").await;
    server.shutdown().await.unwrap();
    drop(fs);
    let fs =
        Arc::new(SharedFs::open(&temp.path().join("share"), &temp.path().join("state")).unwrap());
    let config = NfsConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        rpcbind: None,
        ..NfsConfig::default()
    };
    let (events, _) = tokio::sync::broadcast::channel(8);
    let server = nfs::start(fs, config, events).await.unwrap();
    // No MOUNT or LOOKUP after reopening. Parent also tests a genuine CLI process restart.
    for transport in [Transport::Tcp, Transport::Udp] {
        let address = endpoint(&server, "nfs", transport);
        read(address, transport, &file, b"nested bytes").await;
        let mut args = Vec::new();
        opaque(&mut args, &dir);
        let reply = invoke(address, transport, 100003, 1, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 0);
        assert_eq!(input.word(), 2); // NF3DIR
        input.bytes(80);
        input.end();
        hyper(&mut args, 0);
        hyper(&mut args, 0);
        word(&mut args, 4096);
        let reply = invoke(address, transport, 100003, 16, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 0);
        input.post();
        input.bytes(8);
        assert_eq!(input.word(), 1);
        input.hyper();
        assert_eq!(input.opaque(), b"nested");
        input.hyper();
        assert_eq!(input.word(), 0);
        assert_eq!(input.word(), 1);
        input.end();
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn replaced_objects_return_stale_without_retargeting_old_handles() {
    let (temp, _fs, server) = fixture().await;
    let address = endpoint(&server, "nfs", Transport::Udp);
    let root = mount(address, Transport::Udp).await;
    let old = lookup(address, Transport::Udp, &root, b"hello").await;
    // Keep the old object alive under another name so its native identity cannot be recycled.
    std::fs::rename(
        temp.path().join("share/hello"),
        temp.path().join("old-outside-export"),
    )
    .unwrap();
    std::fs::write(temp.path().join("share/hello"), b"different object").unwrap();
    for transport in [Transport::Tcp, Transport::Udp] {
        let address = endpoint(&server, "nfs", transport);
        let mut args = Vec::new();
        opaque(&mut args, &old);
        let reply = invoke(address, transport, 100003, 1, &args).await;
        let mut input = body(&reply);
        assert_eq!(input.word(), 70);
        input.end();
    }
    server.shutdown().await.unwrap();
}
