//! Process-level coverage: all requests cross real loopback sockets and the CLI
//! is killed/restarted while the client retains its original NFS handles.
use filemux_core::Config;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

struct Daemon {
    child: Child,
    logs: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
    _lines: mpsc::Receiver<String>,
    ready: Value,
}

impl Daemon {
    fn start(config: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_filemux"))
            .arg("--config")
            .arg(config)
            .args(["serve", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sender, lines) = mpsc::channel();
        let out = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let logs = Arc::new(Mutex::new(String::new()));
        let errors = Arc::clone(&logs);
        let err = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut text);
            *errors.lock().unwrap() = text;
        });
        let mut daemon = Self {
            child,
            logs,
            readers: vec![out, err],
            _lines: lines,
            ready: Value::Null,
        };
        let line = match daemon._lines.recv_timeout(Duration::from_secs(20)) {
            Ok(line) => line,
            Err(error) => {
                daemon.kill();
                panic!(
                    "CLI did not become ready: {error}; {}",
                    daemon.logs.lock().unwrap()
                );
            }
        };
        daemon.ready = serde_json::from_str(&line).expect("ready stdout must be JSON");
        assert_eq!(daemon.ready["event"], "ready");
        daemon
    }

    fn endpoint(&self, role: &str, transport: &str) -> SocketAddr {
        self.ready["services"].as_array().unwrap().iter()
            .flat_map(|service| service["endpoints"].as_array().unwrap())
            .find(|endpoint| endpoint["role"] == role && endpoint["transport"] == transport)
            .unwrap_or_else(|| panic!("missing endpoint {role}/{transport}: {}", self.ready))
            ["address"].as_str().unwrap().parse().unwrap()
    }

    fn kill(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        self.child.wait().unwrap();
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn word(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
fn hyper(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
fn opaque(bytes: &mut Vec<u8>, value: &[u8]) {
    word(bytes, value.len() as u32);
    bytes.extend_from_slice(value);
    bytes.resize(bytes.len() + (4 - value.len() % 4) % 4, 0);
}

struct Decode<'a>(&'a [u8]);
impl<'a> Decode<'a> {
    fn take(&mut self, count: usize) -> &'a [u8] {
        let (value, rest) = self.0.split_at(count);
        self.0 = rest;
        value
    }
    fn word(&mut self) -> u32 {
        u32::from_be_bytes(self.take(4).try_into().unwrap())
    }
    fn opaque(&mut self) -> &'a [u8] {
        let length = self.word() as usize;
        let value = self.take(length);
        self.take((4 - length % 4) % 4);
        value
    }
    fn post(&mut self) {
        assert_eq!(self.word(), 1);
        self.take(84);
    }
}

fn rpc(address: SocketAddr, udp: bool, program: u32, procedure: u32, args: &[u8]) -> Vec<u8> {
    let mut packet = Vec::new();
    for value in [0x31415926, 0, 2, program, 3, procedure, 0, 0, 0, 0] {
        word(&mut packet, value);
    }
    packet.extend_from_slice(args);
    let response = if udp {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket.connect(address).unwrap();
        socket.send(&packet).unwrap();
        let mut bytes = vec![0; 65536];
        let length = socket.recv(&mut bytes).unwrap();
        bytes.truncate(length);
        bytes
    } else {
        let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .write_all(&(0x80000000 | packet.len() as u32).to_be_bytes())
            .unwrap();
        socket.write_all(&packet).unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut marker = [0; 4];
            socket.read_exact(&mut marker).unwrap();
            let marker = u32::from_be_bytes(marker);
            let length = (marker & 0x7fffffff) as usize;
            assert!(bytes.len() + length <= 2 * 1024 * 1024);
            let start = bytes.len();
            bytes.resize(start + length, 0);
            socket.read_exact(&mut bytes[start..]).unwrap();
            if marker & 0x80000000 != 0 {
                break;
            }
        }
        bytes
    };
    let mut input = Decode(&response);
    assert_eq!(input.word(), 0x31415926);
    assert_eq!(input.word(), 1); // REPLY
    assert_eq!(input.word(), 0); // MSG_ACCEPTED
    assert_eq!(input.word(), 0); // AUTH_NONE verifier
    assert!(input.opaque().is_empty());
    assert_eq!(input.word(), 0); // RPC SUCCESS
    input.0.to_vec()
}

fn mount(address: SocketAddr) -> Vec<u8> {
    let mut args = Vec::new();
    opaque(&mut args, b"/");
    let reply = rpc(address, false, 100005, 1, &args);
    let mut input = Decode(&reply);
    assert_eq!(input.word(), 0);
    input.opaque().to_vec()
}

fn lookup(address: SocketAddr, directory: &[u8], name: &[u8]) -> Vec<u8> {
    let mut args = Vec::new();
    opaque(&mut args, directory);
    opaque(&mut args, name);
    let reply = rpc(address, false, 100003, 3, &args);
    let mut input = Decode(&reply);
    assert_eq!(input.word(), 0);
    input.opaque().to_vec()
}

fn read_file(address: SocketAddr, udp: bool, handle: &[u8]) -> Vec<u8> {
    let mut args = Vec::new();
    opaque(&mut args, handle);
    hyper(&mut args, 0);
    word(&mut args, 4096);
    let reply = rpc(address, udp, 100003, 6, &args);
    let mut input = Decode(&reply);
    assert_eq!(input.word(), 0);
    input.post();
    let count = input.word() as usize;
    assert_eq!(input.word(), 1);
    let bytes = input.opaque();
    assert_eq!(bytes.len(), count);
    bytes.to_vec()
}

fn read_names(address: SocketAddr, handle: &[u8]) -> Vec<Vec<u8>> {
    let mut args = Vec::new();
    opaque(&mut args, handle);
    hyper(&mut args, 0);
    hyper(&mut args, 0);
    word(&mut args, 8192);
    let reply = rpc(address, false, 100003, 16, &args);
    let mut input = Decode(&reply);
    assert_eq!(input.word(), 0);
    input.post();
    input.take(8);
    let mut names = Vec::new();
    while input.word() == 1 {
        input.take(8);
        names.push(input.opaque().to_vec());
        input.take(8);
    }
    assert_eq!(input.word(), 1); // EOF
    names.sort();
    names
}

#[test]
fn old_file_and_directory_handles_survive_a_real_process_restart() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("share");
    std::fs::create_dir_all(root.join("boot")).unwrap();
    std::fs::write(root.join("boot/kernel.bin"), b"first kernel bytes").unwrap();
    let mut config = Config {
        root,
        state_dir: temp.path().join("state"),
        ..Config::default()
    };
    config.http.bind = "127.0.0.1:0".parse().unwrap();
    config.tftp.enabled = true;
    config.tftp.bind = "127.0.0.1:0".parse().unwrap();
    config.nfs.enabled = true;
    config.nfs.bind = "127.0.0.1:0".parse().unwrap();
    config.nfs.rpcbind = Some("127.0.0.1:0".parse().unwrap());
    let path = temp.path().join("filemux.toml");
    config.save(&path).unwrap();
    let mut first = Daemon::start(&path);
    let address = first.endpoint("nfs", "tcp");
    let root = mount(address);
    let directory = lookup(address, &root, b"boot");
    let file = lookup(address, &directory, b"kernel.bin");
    assert_eq!(read_file(address, false, &file), b"first kernel bytes");
    assert_eq!(read_names(address, &directory), [b"kernel.bin".to_vec()]);
    // Keep addresses too: a real mounted client reconnects to the same server port.
    config.http.bind = first.endpoint("http", "tcp");
    config.tftp.bind = first.endpoint("tftp", "udp");
    config.nfs.bind = address;
    config.nfs.rpcbind = Some(first.endpoint("portmapper", "udp"));
    first.kill();
    // Update contents in-place, not delete/recreate. This also catches a wrong
    // implementation that uses mutable ctime/mtime as the object's generation.
    std::fs::write(
        config.root.join("boot/kernel.bin"),
        b"new uncached bytes after restart",
    )
    .unwrap();
    std::fs::write(config.root.join("boot/extra.txt"), b"new entry").unwrap();
    config.save(&path).unwrap();
    let second = Daemon::start(&path);
    assert_eq!(second.endpoint("nfs", "tcp"), address);
    // No new MOUNT or LOOKUP happens after the restart.
    assert_eq!(
        read_file(address, false, &file),
        b"new uncached bytes after restart"
    );
    assert_eq!(
        read_file(address, true, &file),
        b"new uncached bytes after restart"
    );
    assert_eq!(
        read_names(address, &directory),
        [b"extra.txt".to_vec(), b"kernel.bin".to_vec()]
    );
    let mut args = Vec::new();
    opaque(&mut args, &root);
    assert_eq!(Decode(&rpc(address, false, 100003, 1, &args)).word(), 0);
}

#[test]
fn init_and_check_do_not_overwrite_existing_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("share");
    std::fs::create_dir(&root).unwrap();
    let path = temp.path().join("filemux.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_filemux"))
        .arg("--config")
        .arg(&path)
        .arg("init")
        .arg("--root")
        .arg(&root)
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = std::fs::read(&path).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_filemux"))
        .arg("--config")
        .arg(&path)
        .arg("init")
        .arg("--root")
        .arg(&root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let output = Command::new(env!("CARGO_BIN_EXE_filemux"))
        .arg("--config")
        .arg(&path)
        .arg("check")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !temp.path().join("state").exists(),
        "check must not start services or initialize state"
    );
    let mut config = Config::load(&path).unwrap();
    config.pxe.enabled = true;
    config.pxe.server_ip = std::net::Ipv4Addr::LOCALHOST;
    config.pxe.entries.push(filemux_core::PxeEntry::Chain {
        id: "boot".into(),
        title: "Boot".into(),
        script: "missing.ipxe".into(),
    });
    config.save(&path).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_filemux"))
        .arg("--config")
        .arg(&path)
        .arg("check")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("undionly.kpxe")
            && text.contains("snponly.efi")
            && text.contains("missing.ipxe"),
        "{text}"
    );
    assert!(
        !config.state_dir.exists(),
        "PXE check must not initialize state"
    );
}
