//! One service implementation for both the command line and the desktop application.

use crate::fs::SharedFs;
use crate::{Config, Endpoint, Event, EventSender, Protocol, ServerHandle, ServiceState, emit};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Clone, Debug, Serialize)]
pub struct ServiceSnapshot {
    pub protocol: Protocol,
    pub state: ServiceState,
    pub endpoints: Vec<Endpoint>,
    pub error: Option<String>,
}

pub struct Engine {
    config: Config,
    fs: Option<Arc<SharedFs>>,
    servers: BTreeMap<Protocol, ServerHandle>,
    snapshots: BTreeMap<Protocol, ServiceSnapshot>,
    events: EventSender,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        let (events, _) = broadcast::channel(512);
        Self::with_events(config, events)
    }

    pub fn with_events(config: Config, events: EventSender) -> Self {
        let snapshots = Protocol::ALL
            .into_iter()
            .map(|protocol| {
                (
                    protocol,
                    ServiceSnapshot {
                        protocol,
                        state: ServiceState::Stopped,
                        endpoints: Vec::new(),
                        error: None,
                    },
                )
            })
            .collect();
        Self {
            config,
            fs: None,
            servers: BTreeMap::new(),
            snapshots,
            events,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
    pub fn is_idle(&self) -> bool {
        self.servers.is_empty()
    }
    pub fn snapshot(&self) -> Vec<ServiceSnapshot> {
        Protocol::ALL
            .iter()
            .map(|protocol| self.snapshots[protocol].clone())
            .collect()
    }

    /// Settings take effect together. Do not silently serve different roots from different protocols.
    pub fn configure(&mut self, config: Config) -> Result<()> {
        config.validate()?;
        if config == self.config {
            return Ok(());
        }
        ensure!(
            self.is_idle(),
            "其他服务仍在使用原设置，请先全部停止，再使用新设置"
        );
        if config.root != self.config.root || config.state_dir != self.config.state_dir {
            self.fs = None;
        }
        self.config = config;
        for snapshot in self.snapshots.values_mut() {
            snapshot.error = None;
            snapshot.state = ServiceState::Stopped;
        }
        Ok(())
    }

    async fn filesystem(&mut self) -> Result<Arc<SharedFs>> {
        if let Some(fs) = &self.fs {
            return Ok(Arc::clone(fs));
        }
        let root = self.config.root.clone();
        let state_dir = self.config.state_dir.clone();
        let fs = tokio::task::spawn_blocking(move || SharedFs::open(&root, &state_dir))
            .await
            .context("共享目录初始化任务异常")??;
        let fs = Arc::new(fs);
        self.fs = Some(Arc::clone(&fs));
        Ok(fs)
    }

    pub async fn start(&mut self, protocol: Protocol) -> Result<()> {
        self.check_health().await?;
        if self.servers.contains_key(&protocol) {
            return Ok(());
        }
        {
            let snapshot = self.snapshots.get_mut(&protocol).expect("known protocol");
            snapshot.state = ServiceState::Starting;
            snapshot.error = None;
            snapshot.endpoints.clear();
        }
        let result = self.launch(protocol).await;
        match result {
            Ok(server) => {
                let snapshot = self.snapshots.get_mut(&protocol).expect("known protocol");
                snapshot.state = ServiceState::Running;
                snapshot.endpoints.clone_from(&server.endpoints);
                let listeners = server
                    .endpoints
                    .iter()
                    .map(|endpoint| {
                        format!(
                            "{} {} {}",
                            endpoint.role, endpoint.transport, endpoint.address
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("，");
                self.servers.insert(protocol, server);
                emit(
                    &self.events,
                    protocol,
                    format!("服务已启动：{listeners}"),
                    false,
                );
                Ok(())
            }
            Err(error) => {
                let message = format!("{error:#}");
                let snapshot = self.snapshots.get_mut(&protocol).expect("known protocol");
                snapshot.state = ServiceState::Failed;
                snapshot.error = Some(message.clone());
                emit(&self.events, protocol, format!("启动失败：{message}"), true);
                Err(error)
            }
        }
    }

    async fn launch(&mut self, protocol: Protocol) -> Result<ServerHandle> {
        self.config.validate()?;
        if protocol == Protocol::ProxyDhcp {
            ensure!(
                self.config.pxe.enabled && self.config.pxe.mode == crate::PxeMode::ProxyDhcp,
                "请在 PXE 设置中启用 ProxyDHCP 模式"
            );
            ensure!(
                [Protocol::Http, Protocol::Tftp]
                    .iter()
                    .all(|dependency| self.servers.contains_key(dependency)
                        && self.snapshots[dependency].state == ServiceState::Running),
                "请先启动 HTTP 和 TFTP，再启动 ProxyDHCP"
            );
            return crate::pxe::proxy::start(
                self.config.pxe.clone(),
                self.config.http.bind.port(),
                self.events.clone(),
            )
            .await
            .with_context(|| format!("无法启动 {protocol}"));
        }
        let fs = self.filesystem().await?;
        let events = self.events.clone();
        match protocol {
            Protocol::Http => {
                let pxe = if self.config.pxe.enabled {
                    let config = self.config.clone();
                    let shared = Arc::clone(&fs);
                    Some(
                        tokio::task::spawn_blocking(move || crate::pxe::prepare(&config, &shared))
                            .await
                            .context("PXE 菜单准备任务异常")??,
                    )
                } else {
                    None
                };
                crate::http::start(fs, self.config.http.clone(), pxe, events).await
            }
            Protocol::Tftp => crate::tftp::start(fs, self.config.tftp.clone(), events).await,
            Protocol::Nfs => crate::nfs::start(fs, self.config.nfs.clone(), events).await,
            Protocol::ProxyDhcp => unreachable!("handled before filesystem initialization"),
        }
        .with_context(|| format!("无法启动 {protocol}"))
    }

    /// A CLI startup is all-or-nothing: do not leave an unnoticed partially started configuration.
    pub async fn start_enabled(&mut self) -> Result<()> {
        ensure!(
            !self.config.pxe.enabled || (self.config.http.enabled && self.config.tftp.enabled),
            "启用 PXE 时必须同时启用 HTTP 和 TFTP"
        );
        let requested = [
            (Protocol::Http, self.config.http.enabled),
            (Protocol::Tftp, self.config.tftp.enabled),
            (Protocol::Nfs, self.config.nfs.enabled),
            (
                Protocol::ProxyDhcp,
                self.config.pxe.enabled && self.config.pxe.mode == crate::PxeMode::ProxyDhcp,
            ),
        ];
        ensure!(
            requested.iter().any(|(_, enabled)| *enabled),
            "没有启用任何服务"
        );
        for (protocol, enabled) in requested {
            if enabled {
                if let Err(error) = self.start(protocol).await {
                    let stop_result = self.stop_all().await;
                    return match stop_result {
                        Ok(()) => Err(error),
                        Err(stop_error) => {
                            Err(error
                                .context(format!("同时停止其他服务时发生错误：{stop_error:#}")))
                        }
                    };
                }
            }
        }
        Ok(())
    }

    pub async fn stop(&mut self, protocol: Protocol) -> Result<()> {
        ensure!(
            !matches!(protocol, Protocol::Http | Protocol::Tftp)
                || !self.servers.contains_key(&Protocol::ProxyDhcp),
            "请先停止 ProxyDHCP"
        );
        let Some(server) = self.servers.remove(&protocol) else {
            return Ok(());
        };
        self.snapshots
            .get_mut(&protocol)
            .expect("known protocol")
            .state = ServiceState::Stopping;
        let result = server.shutdown().await;
        let snapshot = self.snapshots.get_mut(&protocol).expect("known protocol");
        snapshot.endpoints.clear();
        match result {
            Ok(()) => {
                snapshot.state = ServiceState::Stopped;
                snapshot.error = None;
                emit(&self.events, protocol, "服务已停止", false);
                Ok(())
            }
            Err(error) => {
                snapshot.state = ServiceState::Failed;
                snapshot.error = Some(format!("{error:#}"));
                emit(
                    &self.events,
                    protocol,
                    format!("停止服务时发生错误：{error:#}"),
                    true,
                );
                Err(error)
            }
        }
    }

    pub async fn stop_all(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for protocol in Protocol::ALL.into_iter().rev() {
            if let Err(error) = self.stop(protocol).await {
                errors.push(format!("{error:#}"));
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("；"));
        }
        Ok(())
    }

    /// Reap listeners that exited unexpectedly; a stale GUI snapshot must not say 'running'.
    pub async fn check_health(&mut self) -> Result<()> {
        let finished: Vec<_> = self
            .servers
            .iter()
            .filter_map(|(protocol, server)| server.is_finished().then_some(*protocol))
            .collect();
        let mut errors = Vec::new();
        if finished
            .iter()
            .any(|protocol| matches!(protocol, Protocol::Http | Protocol::Tftp))
        {
            if let Some(proxy) = self.servers.remove(&Protocol::ProxyDhcp) {
                let stopped = proxy.shutdown().await;
                let mut error = "ProxyDHCP 已停止：HTTP 或 TFTP 依赖服务已停止".to_string();
                if let Err(stop_error) = stopped {
                    error.push_str(&format!("；{stop_error:#}"));
                }
                let snapshot = self
                    .snapshots
                    .get_mut(&Protocol::ProxyDhcp)
                    .expect("known protocol");
                snapshot.state = ServiceState::Failed;
                snapshot.endpoints.clear();
                snapshot.error = Some(error.clone());
                emit(&self.events, Protocol::ProxyDhcp, &error, true);
                errors.push(error);
            }
        }
        for protocol in finished {
            let Some(server) = self.servers.remove(&protocol) else {
                continue;
            };
            let error = match server.shutdown().await {
                Ok(()) => format!("{protocol} 服务意外停止"),
                Err(error) => format!("{protocol} 服务异常退出：{error:#}"),
            };
            let snapshot = self.snapshots.get_mut(&protocol).expect("known protocol");
            snapshot.state = ServiceState::Failed;
            snapshot.endpoints.clear();
            snapshot.error = Some(error.clone());
            emit(&self.events, protocol, &error, true);
            errors.push(error);
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("；"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pxe_dependencies_fail_before_opening_filesystem_or_listeners() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("share");
        std::fs::create_dir(&root).unwrap();
        let mut config = Config {
            root,
            state_dir: dir.path().join("state"),
            ..Config::default()
        };
        config.pxe.enabled = true;
        config.pxe.mode = crate::PxeMode::ProxyDhcp;
        config.pxe.server_ip = std::net::Ipv4Addr::LOCALHOST;
        config.pxe.entries.push(crate::PxeEntry::Chain {
            id: "boot".into(),
            title: "Boot".into(),
            script: "boot.ipxe".into(),
        });
        config.http.enabled = false;
        let mut engine = Engine::new(config.clone());
        assert!(engine.start_enabled().await.is_err());
        assert!(engine.is_idle());
        assert!(!config.state_dir.exists());
        assert!(engine.start(Protocol::ProxyDhcp).await.is_err());
        assert!(engine.is_idle());
        assert!(!config.state_dir.exists());
    }
}
