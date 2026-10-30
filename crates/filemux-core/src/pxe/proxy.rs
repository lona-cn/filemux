//! A lease-free PXE proxy for one explicitly selected IPv4 interface.
//!
//! UDP 67 advertises boot information alongside an independent DHCP server;
//! UDP 4011 answers PXE boot-service requests. Neither endpoint assigns addresses.

use crate::{Endpoint, EventSender, Protocol, PxeConfig, ServerHandle, Transport, emit};
use anyhow::{Context, Result, ensure};
use socket_pktinfo::{PktInfo, PktInfoUdpSocket};
use socket2::Domain;
use std::io;
#[cfg(test)]
use std::net::UdpSocket;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const HEADER_LEN: usize = 240;
const COOKIE: [u8; 4] = [99, 130, 83, 99];
const IO_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Clone, Copy)]
enum Role {
    Proxy,
    Boot,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Proxy => "proxy-dhcp",
            Self::Boot => "pxe-boot",
        }
    }
}

struct Settings {
    server_ip: Ipv4Addr,
    server_name: String,
    mask: u32,
    interface_index: u32,
    bios_bootfile: String,
    uefi_bootfile: String,
    menu_url: String,
    client_port: u16,
}

impl Settings {
    fn client_address(&self, address: Ipv4Addr) -> bool {
        if address.is_unspecified() || address.is_multicast() || address.is_broadcast() {
            return false;
        }
        let address = u32::from(address);
        let network = u32::from(self.server_ip) & self.mask;
        if address & self.mask != network {
            return false;
        }
        // /31 and /32 interfaces have no separate network/broadcast host slots.
        let hosts = !self.mask;
        hosts <= 1 || (address != network && address != network | hosts)
    }
}

/// Bind both standard PXE ports before returning a ready service handle.
/// An independent DHCP server is still required to allocate client addresses.
pub async fn start(config: PxeConfig, http_port: u16, events: EventSender) -> Result<ServerHandle> {
    start_on(
        config,
        http_port,
        events,
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 67),
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 4011),
        68,
    )
    .await
}

// Kept private: nonstandard ports are for real-socket tests, not product settings.
async fn start_on(
    config: PxeConfig,
    http_port: u16,
    events: EventSender,
    proxy_bind: SocketAddrV4,
    boot_bind: SocketAddrV4,
    client_port: u16,
) -> Result<ServerHandle> {
    let ip = config.server_ip;
    ensure!(
        !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast(),
        "ProxyDHCP 必须使用明确的本机单播 IPv4"
    );
    ensure!(
        http_port != 0 && client_port != 0,
        "PXE HTTP 和客户端端口不能为 0"
    );
    for path in [&config.bios_bootfile, &config.uefi_x64_bootfile] {
        super::resource_path(path)?;
        ensure!(
            path.len() <= 127,
            "启动文件名的 UTF-8 长度不能超过 127 字节：{path}"
        );
    }
    let interface = super::selected_interface(ip, true)?;
    let mask = interface
        .addr
        .iter()
        .find(|address| address.ip() == IpAddr::V4(ip))
        .and_then(|address| address.netmask())
        .and_then(|mask| match mask {
            IpAddr::V4(mask) => Some(mask),
            _ => None,
        })
        .context("ProxyDHCP 接口缺少 IPv4 netmask")?;
    let settings = Arc::new(Settings {
        server_ip: ip,
        server_name: ip.to_string(),
        mask: u32::from(mask),
        interface_index: interface.index,
        bios_bootfile: config.bios_bootfile,
        uefi_bootfile: config.uefi_x64_bootfile,
        menu_url: format!("http://{ip}:{http_port}/.filemux-pxe/menu.ipxe"),
        client_port,
    });
    ensure!(
        settings.menu_url.len() <= 127,
        "PXE 菜单 URL 超过启动文件字段长度"
    );
    let bind = |address, role: Role| {
        bind_socket(address, interface.index).with_context(|| format!(
            "无法启动 ProxyDHCP {}：接口 {}（{ip}，索引 {}），监听 {address}；请检查权限和端口占用，可改用外部 DHCP 模式或另一台主机",
            role.name(), interface.name, interface.index
        ))
    };
    let (proxy, proxy_address) = bind(proxy_bind, Role::Proxy)?;
    let (boot, boot_address) = bind(boot_bind, Role::Boot)?;
    let endpoints = vec![
        Endpoint {
            role: Role::Proxy.name(),
            transport: Transport::Udp,
            address: proxy_address,
        },
        Endpoint {
            role: Role::Boot.name(),
            transport: Transport::Udp,
            address: boot_address,
        },
    ];
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        emit(
            &events,
            Protocol::ProxyDhcp,
            format!(
                "ProxyDHCP 已监听 {proxy_address} / {boot_address}，仅响应接口 {}（{ip}），不分配 IP 地址",
                interface.name
            ),
            false,
        );
        let mut proxy_task = spawn_receiver(
            proxy,
            Role::Proxy,
            settings.clone(),
            events.clone(),
            task_cancel.clone(),
        );
        let mut boot_task = spawn_receiver(
            boot,
            Role::Boot,
            settings,
            events.clone(),
            task_cancel.clone(),
        );
        // A panic must cancel the other blocking loop too. Always await both so
        // shutdown completion also means both sockets have actually been dropped.
        let (proxy_result, boot_result) = tokio::select! {
            result = &mut proxy_task => {
                task_cancel.cancel();
                (result, boot_task.await)
            }
            result = &mut boot_task => {
                task_cancel.cancel();
                (proxy_task.await, result)
            }
        };
        let result: Result<()> = (|| {
            proxy_result.context("ProxyDHCP 67 接收任务异常退出")??;
            boot_result.context("ProxyDHCP 4011 接收任务异常退出")??;
            Ok(())
        })();
        match &result {
            Ok(()) => emit(&events, Protocol::ProxyDhcp, "ProxyDHCP 已停止", false),
            Err(error) => emit(
                &events,
                Protocol::ProxyDhcp,
                format!("ProxyDHCP 已停止：{error:#}"),
                true,
            ),
        }
        result
    });
    Ok(ServerHandle::new(
        Protocol::ProxyDhcp,
        endpoints,
        cancel,
        task,
    ))
}

fn bind_socket(
    address: SocketAddrV4,
    interface_index: u32,
) -> Result<(PktInfoUdpSocket, SocketAddr)> {
    // Deliberately do not enable SO_REUSEADDR/SO_REUSEPORT. A local DHCP server
    // must retain its port instead of silently sharing client traffic with us.
    let socket =
        PktInfoUdpSocket::new(Domain::IPV4).context("无法启用接收接口元信息 IP_PKTINFO")?;
    #[cfg(windows)]
    exclusive_bind(&socket).context("无法启用 UDP 独占绑定")?;
    socket.bind(&address.into()).context("无法绑定 UDP 端口")?;
    #[cfg(not(windows))]
    let handle = socket
        .try_clone_std()
        .context("无法取得 UDP socket 选项句柄")?;
    // Winsock duplicates keep independent options; configure the actual receiver.
    #[cfg(windows)]
    let handle = {
        use std::os::windows::io::{AsRawSocket, BorrowedSocket};
        unsafe { BorrowedSocket::borrow_raw(socket.as_raw_socket()) }
    };
    let std_socket = socket2::SockRef::from(&handle);
    std_socket
        .set_broadcast(true)
        .context("无法启用 SO_BROADCAST")?;
    std_socket
        .set_read_timeout(Some(IO_TIMEOUT))
        .context("无法设置 UDP 接收超时")?;
    std_socket
        .set_write_timeout(Some(IO_TIMEOUT))
        .context("无法设置 UDP 发送超时")?;
    select_outgoing_interface(&std_socket, interface_index).context("无法固定 UDP 出站接口")?;
    let local = std_socket
        .local_addr()
        .context("无法读取 UDP 实际监听地址")?
        .as_socket()
        .context("UDP 监听地址不是 IP socket")?;
    Ok((socket, local))
}

#[cfg(windows)]
fn exclusive_bind(socket: &PktInfoUdpSocket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        SO_EXCLUSIVEADDRUSE, SOL_SOCKET, WSAGetLastError, setsockopt,
    };
    let enabled = 1_i32;
    let result = unsafe {
        setsockopt(
            socket.as_raw_socket() as _,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&enabled as *const i32).cast(),
            std::mem::size_of_val(&enabled) as i32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
    }
}

#[cfg(target_os = "linux")]
fn select_outgoing_interface(socket: &socket2::Socket, index: u32) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let index = index.to_be();
    // IP_UNICAST_IF takes an interface index in network byte order, unlike
    // multicast-only IP_MULTICAST_IF. The borrowed descriptor stays owned by std.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_UNICAST_IF,
            (&index as *const u32).cast(),
            std::mem::size_of_val(&index) as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn select_outgoing_interface(socket: &socket2::Socket, index: u32) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        IP_UNICAST_IF, IPPROTO_IP, WSAGetLastError, setsockopt,
    };
    let index = index.to_be();
    let result = unsafe {
        setsockopt(
            socket.as_raw_socket() as _,
            IPPROTO_IP,
            IP_UNICAST_IF,
            (&index as *const u32).cast(),
            std::mem::size_of_val(&index) as i32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
    }
}

#[cfg(target_os = "macos")]
fn select_outgoing_interface(socket: &socket2::Socket, index: u32) -> io::Result<()> {
    let index = std::num::NonZeroU32::new(index)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "接口索引不能为 0"))?;
    socket.bind_device_by_index_v4(Some(index))
}

fn spawn_receiver(
    socket: PktInfoUdpSocket,
    role: Role,
    settings: Arc<Settings>,
    events: EventSender,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::task::spawn_blocking(move || {
        let result = receive_loop(socket, role, &settings, &events, &cancel);
        cancel.cancel();
        result
    })
}

fn receive_loop(
    socket: PktInfoUdpSocket,
    role: Role,
    settings: &Settings,
    events: &EventSender,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut incoming = [0; 4096];
    let mut response = [0; 1024];
    while !cancel.is_cancelled() {
        let (length, info) = match receive(&socket, &mut incoming) {
            Ok(received) => received,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) if oversized_datagram(&error) => continue,
            // A zero-byte datagram is invalid DHCP. The Unix dependency maps
            // recvmsg == 0 to last_os_error even though no OS error occurred.
            Err(error) if error.raw_os_error() == Some(0) => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "{} 接收失败（接口索引 {}）",
                        role.name(),
                        settings.interface_index
                    )
                });
            }
        };
        // The dependency does not expose MSG_TRUNC on Unix. Reject a full
        // buffer conservatively so a truncated datagram is never parsed as valid.
        if length >= incoming.len() || !selected_metadata(&info, settings.interface_index)? {
            continue;
        }
        let Some(request) = Request::parse(&incoming[..length]) else {
            continue;
        };
        let Some(reply) = select_reply(&request, &info, role, settings) else {
            continue;
        };
        let length = encode_reply(&request, &reply, settings, &mut response)
            .context("ProxyDHCP 响应超出固定缓冲区")?;
        if cancel.is_cancelled() {
            break;
        }
        let sent = socket
            .send_to(&response[..length], &reply.destination.into())
            .with_context(|| format!("{} 向 {} 发送失败", role.name(), reply.destination))?;
        ensure!(sent == length, "ProxyDHCP UDP 响应未完整发送");
        emit(
            events,
            Protocol::ProxyDhcp,
            format!(
                "{} {} → {}：{}",
                role.name(),
                if reply.message_type == 2 {
                    "OFFER"
                } else {
                    "ACK"
                },
                reply.destination,
                reply.bootfile
            ),
            false,
        );
    }
    Ok(())
}

fn receive(socket: &PktInfoUdpSocket, buffer: &mut [u8]) -> io::Result<(usize, PktInfo)> {
    // Clear a previous Winsock error so custom missing-PKTINFO errors cannot be
    // mistaken for an earlier timeout. WSARecvMsg sets it on actual I/O failure.
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::Networking::WinSock::WSASetLastError(0);
    }
    // The Unix dependency treats an empty UDP datagram as last_os_error.
    // Reset errno so that case stays a discarded malformed packet, not a stale
    // fatal error left by an unrelated earlier operation on this blocking thread.
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = 0;
    }
    let result = socket.recv(buffer);
    // socket-pktinfo 0.4.1 reports recv errors with last_os_error on Windows;
    // Winsock's authoritative error is WSAGetLastError, including WSAEMSGSIZE.
    #[cfg(windows)]
    if result.is_err() {
        let code = unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() };
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code));
        }
    }
    result
}

fn oversized_datagram(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(windows_sys::Win32::Networking::WinSock::WSAEMSGSIZE)
    }
    #[cfg(not(windows))]
    {
        error.raw_os_error() == Some(libc::EMSGSIZE)
    }
}

fn selected_metadata(info: &PktInfo, selected_index: u32) -> Result<bool> {
    ensure!(
        info.if_index != 0,
        "ProxyDHCP 收到缺少接口索引的报文，已停止而非响应所有网卡"
    );
    Ok(info.if_index == u64::from(selected_index)
        && info.addr_src.is_ipv4()
        && info.addr_dst.is_ipv4())
}

#[derive(Default)]
struct Options<'a> {
    overload: Option<&'a [u8]>,
    message_type: Option<&'a [u8]>,
    server_id: Option<&'a [u8]>,
    vendor: Option<&'a [u8]>,
    architecture: Option<&'a [u8]>,
    ndi: Option<&'a [u8]>,
    uuid: Option<&'a [u8]>,
    user_class: Option<&'a [u8]>,
    ipxe: Option<&'a [u8]>,
    pxe: Option<&'a [u8]>,
}

// Repeated singleton options are accepted only if byte-identical. We never
// concatenate contradictory values or retain any per-client state.
fn singleton<'a>(slot: &mut Option<&'a [u8]>, value: &'a [u8]) -> Option<()> {
    if slot.is_some_and(|previous| previous != value) {
        return None;
    }
    *slot = Some(value);
    Some(())
}

fn tlvs<'a>(mut bytes: &'a [u8], mut visit: impl FnMut(u8, &'a [u8]) -> Option<()>) -> Option<()> {
    while let Some((&tag, rest)) = bytes.split_first() {
        bytes = rest;
        match tag {
            0 => continue,
            255 => return Some(()),
            _ => {}
        }
        let (&length, rest) = bytes.split_first()?;
        let length = usize::from(length);
        let value = rest.get(..length)?;
        bytes = rest.get(length..)?;
        visit(tag, value)?;
    }
    Some(())
}

impl<'a> Options<'a> {
    fn read(&mut self, bytes: &'a [u8], primary: bool) -> Option<()> {
        tlvs(bytes, |tag, value| {
            let slot = match tag {
                43 => &mut self.pxe,
                52 if primary => &mut self.overload,
                52 => return None, // RFC 2132: overload appears only in options.
                53 => &mut self.message_type,
                54 => &mut self.server_id,
                60 => &mut self.vendor,
                77 => &mut self.user_class,
                93 => &mut self.architecture,
                94 => &mut self.ndi,
                97 => &mut self.uuid,
                175 => &mut self.ipxe,
                _ => return Some(()),
            };
            singleton(slot, value)
        })
    }
}

struct Request<'a> {
    header: &'a [u8],
    message_type: u8,
    server_id: Option<Ipv4Addr>,
    ciaddr: Ipv4Addr,
    architecture: u16,
    ipxe: bool,
    boot_item: Option<&'a [u8]>,
    ndi: Option<&'a [u8]>,
    uuid: Option<&'a [u8]>,
}

impl<'a> Request<'a> {
    fn parse(packet: &'a [u8]) -> Option<Self> {
        let header = packet.get(..HEADER_LEN)?;
        if header[0] != 1
            || header[1] != 1
            || header[2] != 6
            || header[236..240] != COOKIE
            || header[24..28] != [0; 4]
        {
            return None;
        }
        let mut options = Options::default();
        options.read(&packet[HEADER_LEN..], true)?;
        if let Some(overload) = options.overload {
            if overload.len() != 1 || !(1..=3).contains(&overload[0]) {
                return None;
            }
            if overload[0] & 1 != 0 {
                options.read(&header[108..236], false)?;
            }
            if overload[0] & 2 != 0 {
                options.read(&header[44..108], false)?;
            }
        }
        let [message_type] = options.message_type? else {
            return None;
        };
        let server_id = match options.server_id {
            Some(value) => Some(Ipv4Addr::from(<[u8; 4]>::try_from(value).ok()?)),
            None => None,
        };
        let architectures = options.architecture?;
        if architectures.is_empty() || architectures.len() % 2 != 0 {
            return None;
        }
        // Current IANA assigns 7 to x64 and 9 to EBC. Accept historical RFC
        // 4578 value 9 for x64 compatibility, never treat arbitrary non-BIOS
        // values (IA32, ARM64, HTTP Boot, etc.) as x64 firmware.
        let architecture = architectures
            .chunks_exact(2)
            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
            .find(|value| matches!(value, 0 | 7 | 9))?;
        let mut boot_item = None;
        if let Some(pxe) = options.pxe {
            let mut discovery = None;
            tlvs(pxe, |tag, value| match tag {
                6 if value.len() == 1 => singleton(&mut discovery, value),
                6 => None,
                71 if value.len() == 4 => singleton(&mut boot_item, value),
                71 => None,
                _ => Some(()),
            })?;
        }
        // Only the default boot service, layer zero; no BIS credential layers.
        if boot_item.is_some_and(|item| item != [0, 0, 0, 0]) {
            return None;
        }
        let user_ipxe = match options.user_class {
            Some(b"iPXE") => true,
            Some(mut classes) => {
                let mut found = false;
                while !classes.is_empty() {
                    let (&length, rest) = classes.split_first()?;
                    if length == 0 {
                        return None;
                    }
                    let class = rest.get(..usize::from(length))?;
                    found |= class == b"iPXE";
                    classes = rest.get(usize::from(length)..)?;
                }
                found
            }
            None => false,
        };
        let mut http_feature = None;
        if let Some(features) = options.ipxe {
            tlvs(features, |tag, value| {
                if tag == 0x13 {
                    if value.len() != 1 {
                        return None;
                    }
                    singleton(&mut http_feature, value)?;
                }
                Some(())
            })?;
        }
        let ipxe = user_ipxe || http_feature.is_some_and(|value| value[0] != 0);
        let pxe = options
            .vendor
            .is_some_and(|vendor| vendor == b"PXEClient" || vendor.starts_with(b"PXEClient:"));
        if !pxe && !ipxe {
            return None;
        }
        if options
            .ndi
            .is_some_and(|value| value.len() != 3 || value[0] != 1)
            || options
                .uuid
                .is_some_and(|value| value.len() != 17 || value[0] != 0)
        {
            return None;
        }
        Some(Self {
            header,
            message_type: *message_type,
            server_id,
            ciaddr: Ipv4Addr::new(header[12], header[13], header[14], header[15]),
            architecture,
            ipxe,
            boot_item,
            ndi: options.ndi,
            uuid: options.uuid,
        })
    }
}

struct Reply<'a> {
    message_type: u8,
    bootfile: &'a str,
    destination: SocketAddrV4,
    echo_client_address: bool,
}

fn select_reply<'a>(
    request: &Request<'_>,
    info: &PktInfo,
    role: Role,
    settings: &'a Settings,
) -> Option<Reply<'a>> {
    let SocketAddr::V4(peer) = info.addr_src else {
        return None;
    };
    if (!request.ciaddr.is_unspecified() && !settings.client_address(request.ciaddr))
        || (!peer.ip().is_unspecified() && !settings.client_address(*peer.ip()))
        || request
            .server_id
            .is_some_and(|server| server != settings.server_ip)
    {
        return None;
    }
    let message_type = match (role, request.message_type) {
        (Role::Proxy, 1) => 2,
        (Role::Boot, 3 | 8) => 5,
        (Role::Proxy, 3 | 8) if request.boot_item.is_some() && !request.ciaddr.is_unspecified() => {
            5
        }
        _ => return None,
    };
    let broadcast = request.header[10] & 0x80 != 0;
    let destination = if matches!(role, Role::Boot) && !peer.ip().is_unspecified() {
        if peer.port() == 0 {
            return None;
        }
        peer
    } else if peer.ip().is_unspecified() || broadcast {
        SocketAddrV4::new(Ipv4Addr::BROADCAST, settings.client_port)
    } else {
        SocketAddrV4::new(*peer.ip(), settings.client_port)
    };
    let bootfile = if request.ipxe {
        &settings.menu_url
    } else if request.architecture == 0 {
        &settings.bios_bootfile
    } else {
        &settings.uefi_bootfile
    };
    Some(Reply {
        message_type,
        bootfile,
        destination,
        echo_client_address: message_type == 5
            && request.boot_item.is_some()
            && !request.ciaddr.is_unspecified(),
    })
}

fn encode_reply(
    request: &Request<'_>,
    reply: &Reply<'_>,
    settings: &Settings,
    buffer: &mut [u8; 1024],
) -> Option<usize> {
    buffer[..HEADER_LEN].fill(0);
    buffer[0..3].copy_from_slice(&[2, 1, 6]);
    buffer[4..8].copy_from_slice(&request.header[4..8]); // xid
    buffer[10..16].copy_from_slice(&request.header[10..16]); // flags, ciaddr
    if reply.echo_client_address {
        buffer[16..20].copy_from_slice(&request.ciaddr.octets());
    }
    buffer[20..24].copy_from_slice(&settings.server_ip.octets());
    buffer[28..44].copy_from_slice(&request.header[28..44]);
    buffer[44..44 + settings.server_name.len()].copy_from_slice(settings.server_name.as_bytes());
    buffer[108..108 + reply.bootfile.len()].copy_from_slice(reply.bootfile.as_bytes());
    buffer[236..240].copy_from_slice(&COOKIE);
    let mut cursor = HEADER_LEN;
    let mut option = |tag: u8, value: &[u8]| -> Option<()> {
        let length = u8::try_from(value.len()).ok()?;
        let target = buffer.get_mut(cursor..cursor + value.len() + 2)?;
        target[0] = tag;
        target[1] = length;
        target[2..].copy_from_slice(value);
        cursor += target.len();
        Some(())
    };
    option(53, &[reply.message_type])?;
    option(54, &settings.server_ip.octets())?;
    option(60, b"PXEClient")?;
    option(93, &request.architecture.to_be_bytes())?;
    option(66, settings.server_name.as_bytes())?;
    option(67, reply.bootfile.as_bytes())?;
    if reply.message_type == 5 && request.boot_item.is_some() {
        option(43, &[6, 1, 0x0b, 71, 4, 0, 0, 0, 0])?;
    } else {
        option(43, &[6, 1, 0x0b])?;
    }
    if let Some(ndi) = request.ndi {
        option(94, ndi)?;
    }
    if let Some(uuid) = request.uuid {
        option(97, uuid)?;
    }
    *buffer.get_mut(cursor)? = 255;
    let length = (cursor + 1).max(300);
    buffer.get_mut(cursor + 1..length)?.fill(0);
    Some(length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket as AsyncUdpSocket;
    use tokio::sync::broadcast;

    // This encoder and the reply decoder deliberately do not use the production
    // parser/writer, so real UDP tests check the actual protocol bytes.
    fn client_packet(
        message: u8,
        architectures: &[u16],
        extra: &[(u8, &[u8])],
        xid: u32,
    ) -> Vec<u8> {
        let mut packet = vec![0; 240];
        packet[0] = 1;
        packet[1] = 1;
        packet[2] = 6;
        packet[4..8].copy_from_slice(&xid.to_be_bytes());
        packet[28..34].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        packet[236..240].copy_from_slice(&[99, 130, 83, 99]);
        packet.extend_from_slice(&[53, 1, message, 60, 9]);
        packet.extend_from_slice(b"PXEClient");
        if !architectures.is_empty() {
            packet.extend_from_slice(&[93, (architectures.len() * 2) as u8]);
            for architecture in architectures {
                packet.extend_from_slice(&architecture.to_be_bytes());
            }
        }
        for (tag, value) in extra {
            packet.extend_from_slice(&[*tag, value.len() as u8]);
            packet.extend_from_slice(value);
        }
        packet.push(255);
        packet
    }

    fn option(packet: &[u8], tag: u8) -> Option<&[u8]> {
        let mut cursor = 240;
        while cursor < packet.len() {
            let current = packet[cursor];
            cursor += 1;
            if current == 255 {
                return None;
            }
            if current == 0 {
                continue;
            }
            let length = usize::from(*packet.get(cursor)?);
            cursor += 1;
            let value = packet.get(cursor..cursor + length)?;
            if current == tag {
                return Some(value);
            }
            cursor += length;
        }
        None
    }

    fn bootfile(packet: &[u8]) -> &str {
        let file = &packet[108..236];
        let end = file
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(file.len());
        std::str::from_utf8(&file[..end]).unwrap()
    }

    async fn received(client: &AsyncUdpSocket) -> Vec<u8> {
        let mut packet = [0; 2048];
        let (length, _) =
            tokio::time::timeout(Duration::from_secs(3), client.recv_from(&mut packet))
                .await
                .expect("PXE response timed out")
                .unwrap();
        packet[..length].to_vec()
    }

    async fn quiet(client: &AsyncUdpSocket) {
        let mut packet = [0; 2048];
        assert!(
            tokio::time::timeout(Duration::from_millis(100), client.recv_from(&mut packet))
                .await
                .is_err(),
            "unexpected PXE response"
        );
    }

    fn assert_reply(
        packet: &[u8],
        message: u8,
        xid: u32,
        file: &str,
        architecture: u16,
        yiaddr: [u8; 4],
    ) {
        assert_eq!(&packet[..3], &[2, 1, 6]);
        assert_eq!(&packet[4..8], &xid.to_be_bytes());
        assert_eq!(&packet[16..20], &yiaddr);
        assert_eq!(&packet[20..24], &[127, 0, 0, 1]);
        assert_eq!(&packet[28..34], &[2, 3, 4, 5, 6, 7]);
        assert_eq!(option(packet, 53), Some(&[message][..]));
        assert_eq!(option(packet, 54), Some(&[127, 0, 0, 1][..]));
        assert_eq!(option(packet, 60), Some(&b"PXEClient"[..]));
        assert_eq!(option(packet, 93), Some(&architecture.to_be_bytes()[..]));
        assert_eq!(option(packet, 66), Some(&b"127.0.0.1"[..]));
        assert_eq!(option(packet, 67), Some(file.as_bytes()));
        assert_eq!(bootfile(packet), file);
        for forbidden in [1, 3, 6, 50, 51, 58, 59] {
            assert!(
                option(packet, forbidden).is_none(),
                "proxy supplied address configuration option {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn real_udp_proxy_boot_architectures_and_rejection_boundaries() {
        let client = AsyncUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let (events, _) = broadcast::channel(32);
        let config = PxeConfig {
            server_ip: Ipv4Addr::LOCALHOST,
            ..PxeConfig::default()
        };
        let handle = start_on(
            config,
            18080,
            events,
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
            client.local_addr().unwrap().port(),
        )
        .await
        .unwrap();
        let proxy_address =
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, handle.endpoints[0].address.port());
        let boot_address =
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, handle.endpoints[1].address.port());
        let menu = "http://127.0.0.1:18080/.filemux-pxe/menu.ipxe";
        let cases: &[(&[u16], Option<(u8, &[u8])>, u16, &str)] = &[
            (&[0], None, 0, "undionly.kpxe"),
            (&[7], None, 7, "snponly.efi"),
            (&[9], None, 9, "snponly.efi"),
            (&[6, 11, 7, 0], None, 7, "snponly.efi"),
            (&[0, 7], None, 0, "undionly.kpxe"),
            (&[0], Some((77, b"iPXE")), 0, menu),
            (&[7], Some((77, b"\x04iPXE")), 7, menu),
            (&[7], Some((175, &[0x13, 1, 1])), 7, menu),
        ];
        for (index, (architectures, ipxe, selected, file)) in cases.iter().enumerate() {
            let xid = 0x1234_0000 + index as u32;
            let extra: Vec<_> = ipxe.iter().copied().collect();
            let packet = client_packet(1, architectures, &extra, xid);
            client.send_to(&packet, proxy_address).await.unwrap();
            let reply = received(&client).await;
            assert_reply(&reply, 2, xid, file, *selected, [0; 4]);
            assert_eq!(option(&reply, 43), Some(&[6, 1, 0x0b][..]));
            // Direct 4011 requests do not need a PXE boot-item suboption.
            for message in [3, 8] {
                let packet = client_packet(message, architectures, &extra, xid + 100);
                client.send_to(&packet, boot_address).await.unwrap();
                assert_reply(
                    &received(&client).await,
                    5,
                    xid + 100,
                    file,
                    *selected,
                    [0; 4],
                );
            }
        }
        let uuid = [0; 17];
        for address in [proxy_address, boot_address] {
            let mut packet = client_packet(
                3,
                &[7],
                &[(43, &[71, 4, 0, 0, 0, 0]), (94, &[1, 3, 0]), (97, &uuid)],
                99,
            );
            packet[12..16].copy_from_slice(&[127, 0, 0, 1]);
            client.send_to(&packet, address).await.unwrap();
            let reply = received(&client).await;
            assert_reply(&reply, 5, 99, "snponly.efi", 7, [127, 0, 0, 1]);
            assert_eq!(
                option(&reply, 43),
                Some(&[6, 1, 0x0b, 71, 4, 0, 0, 0, 0][..])
            );
            assert_eq!(option(&reply, 94), Some(&[1, 3, 0][..]));
            assert_eq!(option(&reply, 97), Some(&uuid[..]));
        }
        let mut normal = client_packet(1, &[0], &[], 1);
        // Remove the independently encoded vendor class, leaving ordinary DHCP.
        normal.drain(243..254);
        let mut malformed = client_packet(1, &[0], &[], 2);
        malformed.pop();
        malformed.extend_from_slice(&[43, 8, 71, 4, 0]);
        let mut relay = client_packet(1, &[0], &[], 3);
        relay[24..28].copy_from_slice(&[127, 0, 0, 1]);
        let mut outside = client_packet(1, &[0], &[], 4);
        outside[12..16].copy_from_slice(&[192, 0, 2, 5]);
        let mut oversized = client_packet(1, &[0], &[], 18);
        oversized.resize(8192, 0);
        let rejected = [
            normal,
            malformed,
            relay,
            outside,
            oversized,
            Vec::new(),
            vec![1; 20],
            client_packet(3, &[0], &[], 5), // Ordinary address request on port 67.
            client_packet(8, &[0], &[], 6),
            client_packet(3, &[0], &[(54, &[192, 0, 2, 1])], 7),
            client_packet(1, &[], &[], 8),
            client_packet(1, &[6, 11, 16], &[], 9),
            client_packet(1, &[0], &[(53, &[3])], 10),
            client_packet(1, &[0], &[(43, &[71, 3, 0, 0, 0])], 11),
            client_packet(1, &[0], &[(43, &[71, 4, 0, 0, 0, 1])], 12),
            client_packet(1, &[0], &[(175, &[0x13, 1, 1, 42, 3, 0])], 13),
        ];
        for packet in rejected {
            client.send_to(&packet, proxy_address).await.unwrap();
            quiet(&client).await;
            client
                .send_to(&client_packet(1, &[0], &[], 0xfeed), proxy_address)
                .await
                .unwrap();
            assert_reply(
                &received(&client).await,
                2,
                0xfeed,
                "undionly.kpxe",
                0,
                [0; 4],
            );
        }
        client
            .send_to(
                &client_packet(3, &[7], &[(54, &[192, 0, 2, 1])], 14),
                boot_address,
            )
            .await
            .unwrap();
        quiet(&client).await;
        client
            .send_to(&client_packet(8, &[7], &[], 15), boot_address)
            .await
            .unwrap();
        assert_reply(&received(&client).await, 5, 15, "snponly.efi", 7, [0; 4]);

        // Options may legally occupy both overloaded fixed fields.
        let mut overloaded = client_packet(1, &[], &[(52, &[3])], 16);
        overloaded[108..115].copy_from_slice(&[0, 93, 4, 0, 6, 0, 7]);
        overloaded[115] = 255;
        overloaded[44..52].copy_from_slice(&[77, 5, 4, b'i', b'P', b'X', b'E', 255]);
        client.send_to(&overloaded, proxy_address).await.unwrap();
        assert_reply(&received(&client).await, 2, 16, menu, 7, [0; 4]);
        // A malformed overloaded TLV is ignored without killing the receiver.
        overloaded[235] = 42;
        overloaded[115] = 0;
        client.send_to(&overloaded, proxy_address).await.unwrap();
        quiet(&client).await;
        client
            .send_to(&client_packet(1, &[0], &[], 17), proxy_address)
            .await
            .unwrap();
        assert_reply(&received(&client).await, 2, 17, "undionly.kpxe", 0, [0; 4]);

        let endpoints = handle.endpoints.clone();
        handle.shutdown().await.unwrap();
        for endpoint in endpoints {
            let rebound = UdpSocket::bind(endpoint.address).unwrap();
            drop(rebound);
        }
    }

    #[test]
    fn interface_metadata_and_unconfigured_destination_are_fail_closed() {
        let info = PktInfo {
            if_index: 8,
            addr_src: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 68).into(),
            addr_dst: Ipv4Addr::BROADCAST.into(),
        };
        assert!(!selected_metadata(&info, 9).unwrap());
        assert!(selected_metadata(&info, 8).unwrap());
        assert!(
            selected_metadata(
                &PktInfo {
                    if_index: 0,
                    ..info.clone()
                },
                8
            )
            .is_err()
        );
        let settings = Settings {
            server_ip: Ipv4Addr::new(192, 0, 2, 1),
            server_name: "192.0.2.1".into(),
            mask: u32::from(Ipv4Addr::new(255, 255, 255, 0)),
            interface_index: 8,
            bios_bootfile: "undionly.kpxe".into(),
            uefi_bootfile: "snponly.efi".into(),
            menu_url: "http://192.0.2.1/menu.ipxe".into(),
            client_port: 68,
        };
        let packet = client_packet(1, &[0], &[], 1);
        let request = Request::parse(&packet).unwrap();
        for role in [Role::Proxy, Role::Boot] {
            let mut request = Request::parse(&packet).unwrap();
            if matches!(role, Role::Boot) {
                request.message_type = 3;
            }
            let reply = select_reply(&request, &info, role, &settings).unwrap();
            assert_eq!(
                reply.destination,
                SocketAddrV4::new(Ipv4Addr::BROADCAST, 68)
            );
        }
        let outside = PktInfo {
            addr_src: "198.51.100.1:68".parse().unwrap(),
            ..info.clone()
        };
        assert!(select_reply(&request, &outside, Role::Proxy, &settings).is_none());
        let assigned = PktInfo {
            addr_src: "192.0.2.5:12345".parse().unwrap(),
            ..info
        };
        assert_eq!(
            select_reply(&request, &assigned, Role::Proxy, &settings)
                .unwrap()
                .destination
                .port(),
            68
        );
        let mut boot_request = Request::parse(&packet).unwrap();
        boot_request.message_type = 3;
        assert_eq!(
            select_reply(&boot_request, &assigned, Role::Boot, &settings)
                .unwrap()
                .destination
                .port(),
            12345
        );
        let mut broadcast_packet = packet.clone();
        broadcast_packet[10] = 0x80;
        let broadcast_request = Request::parse(&broadcast_packet).unwrap();
        assert_eq!(
            select_reply(&broadcast_request, &assigned, Role::Proxy, &settings)
                .unwrap()
                .destination,
            SocketAddrV4::new(Ipv4Addr::BROADCAST, 68)
        );
    }
}
