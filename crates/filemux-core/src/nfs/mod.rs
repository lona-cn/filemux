//! Bounded userspace NFSv3 over UDP and TCP, with MOUNTv3 and portmapper v2.
//!
//! The export is anonymous, read-only and intended for a trusted LAN. AUTH_SYS is
//! accepted as an untrusted identity, not checked against host user accounts. Use
//! `vers=3,nolock` (macOS: `nolocks`); NLM/statd, NFSv2/v4 and RPCSEC_GSS are absent.
//! TCP and UDP are served by the same dispatcher. Handles contain a format tag,
//! persistent export identity and persistent object id, never a startup nonce.
//!
//! READ is capped at 1 MiB over TCP and below 60 KiB over UDP; clients should use
//! the FSINFO limits (a 32 KiB rsize is suitable for both). Listings are bounded by
//! SharedFs and 10,000 entries, with verifier-checked pagination. Metadata follows
//! SharedFs: logical size estimates allocation usage; FSSTAT inode counts are
//! unavailable (zero), and writable capacity is zero. MOUNT keeps no client state.
//! COMMIT validates a regular file and performs sync_all, returning IO on a host
//! that cannot flush a read-only descriptor, rather than pretending writes exist.
//! MOUNT accepts real subdirectories below the configured export name, with
//! component-boundary checks and no traversal or symlink following. NFSv3 READ
//! additionally accepts U-Boot's single trailing zero word; all other trailing
//! data, and trailing data on every other procedure, remain malformed requests.
//!
//! Wire code is original, based on RFCs 1057, 1813, 4506 and 5531; no upstream NFS
//! implementation was copied. The only additional dependency is fs2 (MIT/Apache-2.0).

mod nfs;
mod rpc;
mod xdr;

use self::rpc::{Dispatcher, Service};
use crate::fs::SharedFs;
use crate::{Endpoint, EventSender, NfsConfig, Protocol, ServerHandle, Transport, emit};
use anyhow::{Context, Result, ensure};
use std::future::pending;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const UDP_REPLY_LIMIT: usize = 60 * 1024;
const TCP_REPLY_LIMIT: usize = 1024 * 1024 + 256;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FRAGMENTS: usize = 1024;

struct Request {
    bytes: Vec<u8>,
    service: Service,
    maximum_reply: usize,
    reply: oneshot::Sender<Option<Vec<u8>>>,
}

struct Pair {
    tcp: TcpListener,
    udp: Arc<UdpSocket>,
    address: SocketAddr,
}

async fn bind_pair(address: SocketAddr) -> Result<Pair> {
    let tcp = TcpListener::bind(address)
        .await
        .with_context(|| format!("binding RPC TCP {address}"))?;
    let address = tcp.local_addr()?;
    let udp = Arc::new(
        UdpSocket::bind(address)
            .await
            .with_context(|| format!("binding RPC UDP {address}"))?,
    );
    Ok(Pair { tcp, udp, address })
}

/// Bind every configured TCP/UDP listener before returning. Explicit invocation
/// starts the server even if `config.enabled` is false (the frontend owns toggles).
pub async fn start(
    fs: Arc<SharedFs>,
    config: NfsConfig,
    events: EventSender,
) -> Result<ServerHandle> {
    ensure!(
        (1..=1024).contains(&config.max_connections),
        "NFS max_connections must be 1..=1024"
    );
    ensure!(
        (1024..=4 * 1024 * 1024).contains(&config.max_request_bytes),
        "NFS max_request_bytes must be 1024..=4194304"
    );
    ensure!(
        config.export.starts_with('/') && config.export.len() <= 1024,
        "invalid NFS export name"
    );
    ensure!(
        !config.export.as_bytes().contains(&0)
            && !config.export.contains('\\')
            && !config
                .export
                .split('/')
                .any(|part| part == "." || part == ".."),
        "invalid NFS export name"
    );
    let files = bind_pair(config.bind).await?;
    let portmap = match config.rpcbind {
        Some(address) => Some(bind_pair(address).await?),
        None => None,
    };
    let mut endpoints = Vec::new();
    for (role, address) in std::iter::once(("nfs", files.address))
        .chain(portmap.as_ref().map(|pair| ("portmapper", pair.address)))
    {
        endpoints.push(Endpoint {
            role,
            transport: Transport::Tcp,
            address,
        });
        endpoints.push(Endpoint {
            role,
            transport: Transport::Udp,
            address,
        });
    }
    let dispatcher = Arc::new(Dispatcher {
        fs,
        export: config.export,
        nfs_port: files.address.port(),
        portmap_port: portmap.as_ref().map(|pair| pair.address.port()),
    });
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        let result = serve(
            files,
            portmap,
            dispatcher,
            config.max_connections,
            config.max_request_bytes,
            token,
        )
        .await;
        if let Err(error) = &result {
            emit(
                &events,
                Protocol::Nfs,
                format!("NFS server: {error:#}"),
                true,
            );
        }
        result
    });
    Ok(ServerHandle::new(Protocol::Nfs, endpoints, cancel, task))
}

async fn optional_accept(pair: &Option<Pair>) -> io::Result<(TcpStream, SocketAddr)> {
    match pair {
        Some(pair) => pair.tcp.accept().await,
        None => pending().await,
    }
}

async fn optional_recv(pair: &Option<Pair>, bytes: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
    match pair {
        Some(pair) => pair.udp.recv_from(bytes).await,
        None => pending().await,
    }
}

async fn serve(
    files: Pair,
    portmap: Option<Pair>,
    dispatcher: Arc<Dispatcher>,
    connection_limit: usize,
    request_limit: usize,
    cancel: CancellationToken,
) -> Result<()> {
    // Network tasks never own detached dispatch work. Blocking workers live in
    // this supervisor's separate JoinSet and are drained on graceful shutdown.
    let mut connections = JoinSet::new();
    let mut workers = JoinSet::new();
    let (requests, mut receiver) = mpsc::channel::<Request>(connection_limit);
    let mut file_buffer = vec![0_u8; 65536];
    let mut portmap_buffer = vec![0_u8; 65536];
    let result = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Ok(()),
            Some(result) = workers.join_next(), if !workers.is_empty() => {
                if let Err(error) = result { break Err(anyhow::anyhow!("RPC worker panicked: {error}")); }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            request = receiver.recv(), if workers.len() < connection_limit => {
                let Some(request) = request else { break Ok(()); };
                let dispatcher = dispatcher.clone();
                workers.spawn_blocking(move || {
                    let reply = dispatcher.dispatch(&request.bytes, request.service, request.maximum_reply);
                    let _ = request.reply.send(reply);
                });
            }
            incoming = files.tcp.accept() => {
                match incoming {
                    Ok((stream, _)) if connections.len() < connection_limit => {
                        connections.spawn(tcp_connection(stream, Service::Files, requests.clone(), request_limit));
                    }
                    Ok(_) => {} // drop excess TCP connections before allocating record buffers
                    Err(error) => break Err(error.into()),
                }
            }
            incoming = optional_accept(&portmap) => {
                match incoming {
                    Ok((stream, _)) if connections.len() < connection_limit => {
                        connections.spawn(tcp_connection(stream, Service::Portmapper, requests.clone(), request_limit));
                    }
                    Ok(_) => {}
                    Err(error) => break Err(error.into()),
                }
            }
            packet = files.udp.recv_from(&mut file_buffer) => {
                match packet {
                    Ok((length, peer)) if length <= request_limit && connections.len() < connection_limit => {
                        connections.spawn(udp_request(files.udp.clone(), peer, file_buffer[..length].to_vec(), Service::Files, requests.clone()));
                    }
                    Ok(_) => {} // oversized/busy datagrams are dropped; RPC clients may retry
                    Err(error) => break Err(error.into()),
                }
            }
            packet = optional_recv(&portmap, &mut portmap_buffer) => {
                match packet {
                    Ok((length, peer)) if length <= request_limit && connections.len() < connection_limit => {
                        if let Some(pair) = &portmap {
                            connections.spawn(udp_request(pair.udp.clone(), peer, portmap_buffer[..length].to_vec(), Service::Portmapper, requests.clone()));
                        }
                    }
                    Ok(_) => {}
                    Err(error) => break Err(error.into()),
                }
            }
        }
    };
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    drop(receiver); // queued replies are discarded; no new blocking work on shutdown
    while workers.join_next().await.is_some() {}
    result
}

async fn dispatch(
    requests: &mpsc::Sender<Request>,
    bytes: Vec<u8>,
    service: Service,
    maximum_reply: usize,
) -> Option<Vec<u8>> {
    let (reply, receiver) = oneshot::channel();
    requests
        .send(Request {
            bytes,
            service,
            maximum_reply,
            reply,
        })
        .await
        .ok()?;
    receiver.await.ok().flatten()
}

async fn udp_request(
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    bytes: Vec<u8>,
    service: Service,
    requests: mpsc::Sender<Request>,
) {
    if let Some(reply) = dispatch(&requests, bytes, service, UDP_REPLY_LIMIT).await {
        let _ = tokio::time::timeout(IO_TIMEOUT, socket.send_to(&reply, peer)).await;
    }
}

async fn tcp_connection(
    mut socket: TcpStream,
    service: Service,
    requests: mpsc::Sender<Request>,
    maximum: usize,
) {
    loop {
        let bytes = match tokio::time::timeout(IO_TIMEOUT, read_record(&mut socket, maximum)).await
        {
            Ok(Ok(Some(bytes))) => bytes,
            _ => return,
        };
        let Some(reply) = dispatch(&requests, bytes, service, TCP_REPLY_LIMIT).await else {
            return;
        };
        let sent = tokio::time::timeout(IO_TIMEOUT, async {
            socket.write_u32(0x80000000 | reply.len() as u32).await?;
            socket.write_all(&reply).await
        })
        .await;
        if !matches!(sent, Ok(Ok(()))) {
            return;
        }
    }
}

async fn read_record(socket: &mut TcpStream, maximum: usize) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    for fragment in 0..MAX_FRAGMENTS {
        let marker = match socket.read_u32().await {
            Ok(marker) => marker,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof && fragment == 0 => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let length = (marker & 0x7fffffff) as usize;
        if length > maximum.saturating_sub(bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RPC record exceeds configured limit",
            ));
        }
        let start = bytes.len();
        bytes.resize(start + length, 0);
        socket.read_exact(&mut bytes[start..]).await?;
        if marker & 0x80000000 != 0 {
            return Ok(Some(bytes));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "too many RPC fragments",
    ))
}
