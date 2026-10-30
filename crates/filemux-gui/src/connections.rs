use crate::i18n::Language;
use anyhow::{Context, Result};
use filemux_core::engine::ServiceSnapshot;
use filemux_core::{Config, Protocol, ServiceState, Transport};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use std::fmt::Write;
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AccessScope {
    LocalOnly,
    AllIpv4,
    AllIpv6,
    SelectedAddress,
}

impl AccessScope {
    pub(super) fn label(self, language: Language) -> &'static str {
        match self {
            Self::LocalOnly => language.text("This computer only", "仅本机可访问"),
            Self::AllIpv4 => language.text("All IPv4 interfaces", "监听所有 IPv4 接口"),
            Self::AllIpv6 => language.text("All IPv6 interfaces", "监听所有 IPv6 接口"),
            Self::SelectedAddress => {
                language.text("Selected interface address", "通过指定地址访问")
            }
        }
    }
}

pub(super) struct ConnectionInfo {
    pub scope: AccessScope,
    pub targets: Vec<String>,
    pub lookup_failed: bool,
}

impl ConnectionInfo {
    pub(super) fn new(
        snapshot: &ServiceSnapshot,
        config: &Config,
        local_addresses: Result<&[IpAddr], ()>,
    ) -> Option<Self> {
        if snapshot.state != ServiceState::Running {
            return None;
        }
        let role = match snapshot.protocol {
            Protocol::Http => "http",
            Protocol::Tftp => "tftp",
            Protocol::Nfs => "nfs",
            Protocol::ProxyDhcp => return None,
        };
        let endpoint = snapshot
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.role == role)
            .min_by_key(|endpoint| endpoint.transport != Transport::Tcp)?;
        let ip = endpoint.address.ip();
        let scope = if ip.is_loopback() {
            AccessScope::LocalOnly
        } else if ip.is_unspecified() {
            if ip.is_ipv4() {
                AccessScope::AllIpv4
            } else {
                AccessScope::AllIpv6
            }
        } else {
            AccessScope::SelectedAddress
        };
        let mut info = Self {
            scope,
            targets: Vec::new(),
            lookup_failed: false,
        };
        if snapshot.protocol == Protocol::Tftp {
            return Some(info);
        }
        let mut add_target = |target: IpAddr| {
            let address = SocketAddr::new(target, endpoint.address.port());
            info.targets.push(match snapshot.protocol {
                Protocol::Http => format!("http://{address}/"),
                Protocol::Nfs => nfs_command(address, endpoint.transport, &config.nfs.export),
                _ => unreachable!(),
            });
        };
        if ip.is_unspecified() {
            match local_addresses {
                Ok(addresses) => {
                    for &candidate in addresses {
                        if candidate.is_ipv4() == ip.is_ipv4()
                            && !candidate.is_loopback()
                            && usable_ip(candidate)
                        {
                            add_target(candidate);
                        }
                    }
                }
                Err(()) => info.lookup_failed = true,
            }
        } else if usable_ip(ip) {
            add_target(ip);
        }
        Some(info)
    }
}

pub(super) fn local_addresses() -> Result<Vec<IpAddr>> {
    let mut addresses: Vec<_> = NetworkInterface::show()
        .context("Could not read local network interfaces")?
        .into_iter()
        .flat_map(|interface| interface.addr)
        .map(|address| address.ip())
        .filter(|ip| !ip.is_loopback() && usable_ip(*ip))
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    Ok(addresses)
}

fn usable_ip(ip: IpAddr) -> bool {
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match ip {
        IpAddr::V4(ip) => !ip.is_broadcast(),
        // A server's zone index is not a client's zone index, and browsers do not
        // consistently support scoped IPv6 URLs. Do not offer an invalid target.
        IpAddr::V6(ip) => !ip.is_unicast_link_local(),
    }
}

fn nfs_command(address: SocketAddr, transport: Transport, export: &str) -> String {
    let protocol = if transport == Transport::Tcp {
        "tcp"
    } else {
        "udp"
    };
    let port = address.port();
    let mut command = String::with_capacity(180 + export.len());
    write!(command,
        "sudo mount -t nfs -o ro,vers=3,proto={protocol},mountproto={protocol},port={port},mountport={port},nolock,noacl '",
    ).expect("writing to a String");
    match address.ip() {
        IpAddr::V4(ip) => write!(command, "{ip}:"),
        IpAddr::V6(ip) => write!(command, "[{ip}]:"),
    }
    .expect("writing to a String");
    for character in export.chars() {
        if character == '\'' {
            command.push_str("'\\''");
        } else {
            command.push(character);
        }
    }
    command.push_str("' /mnt/filemux");
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use filemux_core::Endpoint;

    fn snapshot(protocol: Protocol, address: &str) -> ServiceSnapshot {
        ServiceSnapshot {
            protocol,
            state: ServiceState::Running,
            endpoints: vec![Endpoint {
                role: if protocol == Protocol::Http {
                    "http"
                } else {
                    "nfs"
                },
                transport: Transport::Tcp,
                address: address.parse().unwrap(),
            }],
            error: None,
        }
    }

    #[test]
    fn wildcard_http_offers_only_matching_unicast_client_addresses() {
        let addresses = [
            "0.0.0.0",
            "127.0.0.1",
            "192.168.1.20",
            "224.0.0.1",
            "255.255.255.255",
            "fd00::20",
        ]
        .map(|ip| ip.parse().unwrap());
        let info = ConnectionInfo::new(
            &snapshot(Protocol::Http, "0.0.0.0:49123"),
            &Config::default(),
            Ok(&addresses),
        )
        .unwrap();
        assert_eq!(info.scope, AccessScope::AllIpv4);
        assert_eq!(info.targets, ["http://192.168.1.20:49123/"]);
    }

    #[test]
    fn ipv6_http_is_bracketed_and_loopback_is_not_advertised_as_lan() {
        let info = ConnectionInfo::new(
            &snapshot(Protocol::Http, "[::1]:49124"),
            &Config::default(),
            Ok(&[]),
        )
        .unwrap();
        assert_eq!(info.scope, AccessScope::LocalOnly);
        assert_eq!(info.targets, ["http://[::1]:49124/"]);
        let scoped = ConnectionInfo::new(
            &snapshot(Protocol::Http, "[fe80::1%3]:49124"),
            &Config::default(),
            Ok(&[]),
        )
        .unwrap();
        assert!(scoped.targets.is_empty());
    }

    #[test]
    fn nfs_uses_the_nfs_endpoint_and_quotes_export_names() {
        let mut snapshot = snapshot(Protocol::Nfs, "127.0.0.1:49125");
        snapshot.endpoints.insert(
            0,
            Endpoint {
                role: "portmapper",
                transport: Transport::Udp,
                address: "127.0.0.1:111".parse().unwrap(),
            },
        );
        let mut config = Config::default();
        config.nfs.export = "/team's boot files".into();
        let info = ConnectionInfo::new(&snapshot, &config, Ok(&[])).unwrap();
        assert!(info.targets[0].contains("port=49125,mountport=49125"));
        assert!(info.targets[0].ends_with("'127.0.0.1:/team'\\''s boot files' /mnt/filemux"));
    }
}
