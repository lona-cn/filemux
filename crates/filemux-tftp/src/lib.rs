//! Bounded, stop-and-wait TFTP (RFC 1350, 2347, 2348 and 2349).
//!
//! Only octet mode is supported. Recognized options are `blksize` (8..=65464,
//! capped by configuration), `timeout` (1..=255 seconds, echoed exactly), and
//! `tsize`. Unknown options, including windowsize, are omitted from the OACK.
//! Upload publication precedes the final ACK; a published upload is never undone
//! merely because its final ACK was lost. Every transfer has its own UDP TID.
//! Requests remain limited to 4096 bytes, but the listener receives full UDP
//! datagrams so oversized requests are rejected without stopping the service.
//! ERROR packets are never answered; only the transfer peer can abort its TID.

use anyhow::{Context, ensure};
use filemux_core::{Endpoint, EventSender, Protocol, ServerHandle, TftpConfig, Transport, emit};
use filemux_fs::{FsError, SharedFs, Upload};
use std::collections::HashSet;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

const DEFAULT_BLOCK: usize = 512;
const MAX_REQUEST: usize = 4096;
const MAX_OPTIONS: usize = 16;

/// Bind the actual listener before returning. Explicit calls start the service
/// even when `config.enabled` is false. Children belong to the returned handle.
pub async fn start(
    fs: Arc<SharedFs>,
    config: TftpConfig,
    events: EventSender,
) -> anyhow::Result<ServerHandle> {
    ensure!(
        (1..=1024).contains(&config.max_transfers),
        "invalid TFTP transfer limit"
    );
    ensure!(
        (100..=60_000).contains(&config.timeout_ms),
        "invalid TFTP timeout"
    );
    ensure!(
        (1..=100).contains(&config.retries),
        "invalid TFTP retry limit"
    );
    ensure!(
        (8..=65_464).contains(&config.block_size_limit),
        "invalid TFTP block size limit"
    );
    ensure!(config.max_upload_bytes > 0, "invalid TFTP upload limit");
    let socket = UdpSocket::bind(config.bind)
        .await
        .context("binding TFTP listener")?;
    let address = socket.local_addr()?;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        let mut active = HashSet::new();
        let mut buffer = vec![0; 65_536];
        emit(
            &events,
            Protocol::Tftp,
            format!("Listening on {address}"),
            false,
        );
        loop {
            tokio::select! {
                biased;
                _ = task_cancel.cancelled() => break,
                result = tasks.join_next(), if !tasks.is_empty() => {
                    match result {
                        Some(Ok(peer)) => { active.remove(&peer); }
                        Some(Err(error)) => {
                            task_cancel.cancel();
                            while tasks.join_next().await.is_some() {}
                            return Err(error).context("TFTP 传输任务异常退出");
                        }
                        None => {}
                    }
                }
                received = socket.recv_from(&mut buffer) => {
                    let (length, peer) = received.context("receiving TFTP request")?;
                    // ERROR packets must not elicit ERROR replies, including
                    // malformed ones with only an opcode.
                    if buffer[..length].starts_with(&[0, 5]) {
                        continue;
                    }
                    // Retransmitted requests must not allocate another TID. The
                    // existing transfer retransmits its outstanding response.
                    if active.contains(&peer) && length >= 2
                        && buffer[0] == 0 && matches!(buffer[1], 1 | 2)
                    {
                        continue;
                    }
                    let request = match Request::parse(&buffer[..length], &config) {
                        Ok(request) => request,
                        Err(error) => {
                            send_error(&socket, peer, &error).await;
                            continue;
                        }
                    };
                    if active.len() >= config.max_transfers {
                        send_error(&socket, peer, &Failure::new(0, "Server busy")).await;
                        continue;
                    }
                    // Binding here ensures failed TID creation is reported on
                    // the listening socket and never leaves an active slot.
                    let tid = match UdpSocket::bind(SocketAddr::new(address.ip(), 0)).await {
                        Ok(tid) => tid,
                        Err(error) => {
                            send_error(&socket, peer, &Failure::from(error)).await;
                            continue;
                        }
                    };
                    active.insert(peer);
                    let fs = fs.clone();
                    let config = config.clone();
                    let events = events.clone();
                    let cancel = task_cancel.clone();
                    tasks.spawn(async move {
                        let direction = if request.write { "Upload" } else { "Download" };
                        emit(&events, Protocol::Tftp, format!("{direction} started: {peer} {:?}", request.path), false);
                        let result = transfer(&tid, peer, fs, &config, request, &cancel).await;
                        match result {
                            Ok(()) => emit(&events, Protocol::Tftp, format!("{direction} completed: {peer}"), false),
                            Err(error) if !cancel.is_cancelled() => {
                                send_error(&tid, peer, &error).await;
                                emit(&events, Protocol::Tftp, format!("{direction} failed: {peer}: {error}"), true);
                            }
                            Err(_) => {}
                        }
                        peer
                    });
                }
            }
        }
        // Cancellation wakes every network wait. Await children rather than
        // detaching their filesystem cleanup or an in-flight atomic commit.
        task_cancel.cancel();
        while tasks.join_next().await.is_some() {}
        emit(&events, Protocol::Tftp, "Stopped", false);
        Ok(())
    });
    Ok(ServerHandle::new(
        Protocol::Tftp,
        vec![Endpoint {
            role: "tftp",
            transport: Transport::Udp,
            address,
        }],
        cancel,
        task,
    ))
}

#[derive(Debug)]
struct Failure {
    code: Option<u16>,
    message: String,
}

impl Failure {
    fn new(code: u16, message: impl Into<String>) -> Self {
        Self {
            code: Some(code),
            message: message.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

fn io_code(kind: io::ErrorKind) -> u16 {
    match kind {
        io::ErrorKind::NotFound => 1,
        io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => 2,
        io::ErrorKind::AlreadyExists => 6,
        io::ErrorKind::StorageFull => 3,
        _ => 0,
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::new(io_code(error.kind()), "Filesystem or socket I/O failed")
    }
}

impl From<FsError> for Failure {
    fn from(error: FsError) -> Self {
        Self::new(io_code(error.kind()), "File unavailable or access denied")
    }
}

impl From<tokio::task::JoinError> for Failure {
    fn from(_: tokio::task::JoinError) -> Self {
        Self::new(0, "Filesystem worker failed")
    }
}

type Result<T, E = Failure> = std::result::Result<T, E>;

struct Request {
    write: bool,
    path: PathBuf,
    block_size: usize,
    timeout: Duration,
    size: Option<u64>,
    options: Vec<(String, String)>,
}

impl Request {
    fn parse(packet: &[u8], config: &TftpConfig) -> Result<Self> {
        if packet.len() < 4 || packet.len() > MAX_REQUEST {
            return Err(Failure::new(4, "Invalid request length"));
        }
        let opcode = u16::from_be_bytes([packet[0], packet[1]]);
        if opcode != 1 && opcode != 2 {
            return Err(Failure::new(5, "Unknown transfer ID"));
        }
        if packet.last() != Some(&0) {
            return Err(Failure::new(4, "Unterminated request"));
        }
        let fields: Vec<&[u8]> = packet[2..packet.len() - 1]
            .split(|byte| *byte == 0)
            .collect();
        if fields.len() < 2 || (fields.len() - 2) % 2 != 0 || fields.len() > 2 + MAX_OPTIONS * 2 {
            return Err(Failure::new(4, "Invalid request fields"));
        }
        let filename = std::str::from_utf8(fields[0])
            .map_err(|_| Failure::new(4, "Filename must be UTF-8"))?;
        if filename.is_empty()
            || filename.len() > 1024
            || filename.contains(['\\', ':'])
            || filename.chars().any(char::is_control)
            || filename.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.ends_with(['.', ' '])
                    || part.to_ascii_lowercase().starts_with(".filemux-")
            })
        {
            return Err(Failure::new(2, "Unsafe filename"));
        }
        if !fields[1].eq_ignore_ascii_case(b"octet") {
            return Err(Failure::new(4, "Only octet mode is supported"));
        }
        let write = opcode == 2;
        if write && !config.allow_upload {
            return Err(Failure::new(2, "Uploads are disabled"));
        }
        let mut request = Self {
            write,
            path: PathBuf::from(filename),
            block_size: DEFAULT_BLOCK,
            timeout: Duration::from_millis(config.timeout_ms),
            size: None,
            options: Vec::new(),
        };
        let mut seen = HashSet::new();
        for pair in fields[2..].chunks_exact(2) {
            let key = std::str::from_utf8(pair[0])
                .map_err(|_| Failure::new(8, "Invalid option"))?
                .to_ascii_lowercase();
            if key.is_empty() || !seen.insert(key.clone()) {
                return Err(Failure::new(8, "Duplicate or empty option"));
            }
            if !matches!(key.as_str(), "blksize" | "timeout" | "tsize") {
                continue;
            }
            let text = std::str::from_utf8(pair[1])
                .map_err(|_| Failure::new(8, "Invalid option value"))?;
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Failure::new(8, "Invalid numeric option"));
            }
            let value: u64 = text
                .parse()
                .map_err(|_| Failure::new(8, "Option value overflow"))?;
            let accepted = match key.as_str() {
                "blksize" if (8..=65_464).contains(&value) => {
                    request.block_size = value.min(u64::from(config.block_size_limit)) as usize;
                    request.block_size as u64
                }
                "timeout" if (1..=255).contains(&value) => {
                    request.timeout = Duration::from_secs(value);
                    value
                }
                "tsize" if write || value == 0 => {
                    if write && value > config.max_upload_bytes {
                        return Err(Failure::new(3, "Upload exceeds size limit"));
                    }
                    request.size = Some(value);
                    value
                }
                _ => return Err(Failure::new(8, "Unsupported option value")),
            };
            request.options.push((key, accepted.to_string()));
        }
        Ok(request)
    }

    fn oack(&self) -> Vec<u8> {
        let mut packet = vec![0, 6];
        for (key, value) in &self.options {
            packet.extend_from_slice(key.as_bytes());
            packet.push(0);
            packet.extend_from_slice(value.as_bytes());
            packet.push(0);
        }
        packet
    }
}

async fn send_error(socket: &UdpSocket, peer: SocketAddr, error: &Failure) {
    let Some(code) = error.code else {
        return;
    };
    let mut packet = Vec::with_capacity(error.message.len() + 5);
    packet.extend_from_slice(&[0, 5]);
    packet.extend_from_slice(&code.to_be_bytes());
    packet.extend_from_slice(error.message.as_bytes());
    packet.push(0);
    let _ = socket.send_to(&packet, peer).await;
}

fn header(opcode: u16, block: u16) -> [u8; 4] {
    let [op_high, op_low] = opcode.to_be_bytes();
    let [high, low] = block.to_be_bytes();
    [op_high, op_low, high, low]
}

fn packet_header(packet: &[u8]) -> Result<(u16, u16)> {
    if packet.len() < 4 {
        return Err(Failure::new(4, "Short transfer packet"));
    }
    let opcode = u16::from_be_bytes([packet[0], packet[1]]);
    Ok((opcode, u16::from_be_bytes([packet[2], packet[3]])))
}

/// A fixed deadline is not extended by wrong-TID or unrelated traffic.
async fn receive(
    socket: &UdpSocket,
    peer: SocketAddr,
    buffer: &mut [u8],
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Option<usize>> {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Failure::new(0, "Transfer cancelled")),
            _ = sleep_until(deadline) => return Ok(None),
            received = socket.recv_from(buffer) => {
                let (length, source) = received?;
                let is_error = buffer[..length].starts_with(&[0, 5]);
                if source != peer {
                    if !is_error {
                        send_error(socket, source, &Failure::new(5, "Unknown transfer ID")).await;
                    }
                    continue;
                }
                if is_error {
                    return Err(Failure {
                        code: None,
                        message: "Client aborted transfer".into(),
                    });
                }
                return Ok(Some(length));
            }
        }
    }
}

async fn await_ack(
    socket: &UdpSocket,
    peer: SocketAddr,
    packet: &[u8],
    block: u16,
    request: &Request,
    retries: u32,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut buffer = [0; 516];
    for _ in 0..=retries {
        if cancel.is_cancelled() {
            return Err(Failure::new(0, "Transfer cancelled"));
        }
        socket.send_to(packet, peer).await?;
        let deadline = Instant::now() + request.timeout;
        while let Some(length) = receive(socket, peer, &mut buffer, deadline, cancel).await? {
            let (opcode, received_block) = packet_header(&buffer[..length])?;
            if opcode != 4 || length != 4 {
                return Err(Failure::new(4, "Expected ACK"));
            }
            if received_block == block {
                return Ok(());
            }
            // Ignore stale ACKs. Resending DATA on every duplicate ACK causes
            // the RFC 1123 sorcerer's-apprentice amplification bug.
        }
    }
    Err(Failure::new(0, "Transfer timed out"))
}

async fn transfer(
    socket: &UdpSocket,
    peer: SocketAddr,
    fs: Arc<SharedFs>,
    config: &TftpConfig,
    mut request: Request,
    cancel: &CancellationToken,
) -> Result<()> {
    if request.write {
        let path = request.path.clone();
        let (upload, writer) = tokio::task::spawn_blocking(move || {
            let upload = fs.begin_upload(&path)?;
            let writer = upload.writer()?;
            Ok::<_, FsError>((upload, writer))
        })
        .await??;
        let mut upload = Some(upload);
        let mut writer = Some(tokio::fs::File::from_std(writer));
        let result = upload_data(
            socket,
            peer,
            &request,
            config,
            cancel,
            &mut upload,
            &mut writer,
        )
        .await;
        // Closing the async writer before removing its temporary file also
        // works on Windows where open-file deletion is restricted.
        if let Some(writer) = writer {
            drop(writer.into_std().await);
        }
        if let Some(upload) = upload {
            tokio::task::spawn_blocking(move || drop(upload)).await?;
        }
        result
    } else {
        let path = request.path.clone();
        let (file, size) = tokio::task::spawn_blocking(move || {
            let file = fs.open_read(&path)?;
            let size = file.metadata().map_err(Failure::from)?.len();
            Ok::<_, Failure>((file, size))
        })
        .await??;
        for (key, value) in &mut request.options {
            if key == "tsize" {
                *value = size.to_string();
            }
        }
        if !request.options.is_empty() {
            await_ack(
                socket,
                peer,
                &request.oack(),
                0,
                &request,
                config.retries,
                cancel,
            )
            .await?;
        }
        let mut file = tokio::fs::File::from_std(file);
        let mut packet = vec![0; request.block_size + 4];
        let mut block = 1u16;
        loop {
            let mut length = 0;
            while length < request.block_size {
                let read = file.read(&mut packet[4 + length..]).await?;
                if read == 0 {
                    break;
                }
                length += read;
            }
            packet[..4].copy_from_slice(&header(3, block));
            await_ack(
                socket,
                peer,
                &packet[..length + 4],
                block,
                &request,
                config.retries,
                cancel,
            )
            .await?;
            if length < request.block_size {
                return Ok(());
            }
            block = block.wrapping_add(1);
        }
    }
}

async fn upload_data(
    socket: &UdpSocket,
    peer: SocketAddr,
    request: &Request,
    config: &TftpConfig,
    cancel: &CancellationToken,
    upload: &mut Option<Upload>,
    writer: &mut Option<tokio::fs::File>,
) -> Result<()> {
    let mut response = if request.options.is_empty() {
        header(4, 0).to_vec()
    } else {
        request.oack()
    };
    let mut buffer = vec![0; request.block_size + 5];
    let mut expected = 1u16;
    let mut total = 0u64;
    loop {
        let mut accepted = None;
        for _ in 0..=config.retries {
            if cancel.is_cancelled() {
                return Err(Failure::new(0, "Transfer cancelled"));
            }
            socket.send_to(&response, peer).await?;
            let deadline = Instant::now() + request.timeout;
            while let Some(length) = receive(socket, peer, &mut buffer, deadline, cancel).await? {
                let (opcode, block) = packet_header(&buffer[..length])?;
                if opcode != 3 || length > request.block_size + 4 {
                    return Err(Failure::new(4, "Expected bounded DATA packet"));
                }
                if block == expected {
                    accepted = Some(length);
                    break;
                }
                if block == expected.wrapping_sub(1) {
                    // Duplicate DATA must be re-ACKed but never written twice.
                    socket.send_to(&response, peer).await?;
                }
            }
            if accepted.is_some() {
                break;
            }
        }
        let length = accepted.ok_or_else(|| Failure::new(0, "Transfer timed out"))?;
        let payload = &buffer[4..length];
        total = total
            .checked_add(payload.len() as u64)
            .ok_or_else(|| Failure::new(3, "Upload too large"))?;
        if total > config.max_upload_bytes || request.size.is_some_and(|size| total > size) {
            return Err(Failure::new(3, "Upload exceeds size limit"));
        }
        writer
            .as_mut()
            .ok_or_else(|| Failure::new(0, "Writer already closed"))?
            .write_all(payload)
            .await?;
        let final_block = payload.len() < request.block_size;
        response.clear();
        response.extend_from_slice(&header(4, expected));
        if final_block {
            if request.size.is_some_and(|size| total != size) {
                return Err(Failure::new(8, "Upload differs from negotiated tsize"));
            }
            let writer = writer
                .take()
                .ok_or_else(|| Failure::new(0, "Writer already closed"))?;
            writer.sync_all().await?;
            // into_std waits for pending Tokio filesystem operations to finish.
            drop(writer.into_std().await);
            if cancel.is_cancelled() {
                return Err(Failure::new(0, "Transfer cancelled"));
            }
            let upload = upload
                .take()
                .ok_or_else(|| Failure::new(0, "Upload already committed"))?;
            tokio::task::spawn_blocking(move || upload.commit()).await??;
            if cancel.is_cancelled() {
                return Err(Failure::new(0, "Transfer cancelled"));
            }
            socket.send_to(&response, peer).await?;
            return dally(socket, peer, expected, payload, &response, request, cancel).await;
        }
        expected = expected.wrapping_add(1);
    }
}

async fn dally(
    socket: &UdpSocket,
    peer: SocketAddr,
    block: u16,
    final_data: &[u8],
    ack: &[u8],
    request: &Request,
    cancel: &CancellationToken,
) -> Result<()> {
    // Two negotiated timeout periods catch a lost final ACK, without extending
    // the lifetime indefinitely when a client repeatedly duplicates DATA.
    let deadline = Instant::now() + request.timeout.saturating_mul(2);
    let mut buffer = vec![0; request.block_size + 5];
    loop {
        let received = receive(socket, peer, &mut buffer, deadline, cancel).await;
        let length = match received {
            Ok(Some(length)) => length,
            Ok(None) => return Ok(()),
            Err(_) if cancel.is_cancelled() => return Ok(()),
            Err(error) => return Err(error),
        };
        if buffer[..length].starts_with(&header(3, block)) && &buffer[4..length] == final_data {
            socket.send_to(ack, peer).await?;
        }
    }
}
