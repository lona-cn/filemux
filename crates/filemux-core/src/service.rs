use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Tftp,
    Nfs,
    ProxyDhcp,
}

impl Protocol {
    pub const ALL: [Self; 4] = [Self::Http, Self::Tftp, Self::Nfs, Self::ProxyDhcp];
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Http => "HTTP",
            Self::Tftp => "TFTP",
            Self::Nfs => "NFSv3",
            Self::ProxyDhcp => "ProxyDHCP",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Tcp,
    Udp,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Endpoint {
    pub role: &'static str,
    pub transport: Transport,
    pub address: SocketAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl ServiceState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "已停止",
            Self::Starting => "启动中",
            Self::Running => "运行中",
            Self::Stopping => "停止中",
            Self::Failed => "启动或运行失败",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub at: SystemTime,
    pub protocol: Protocol,
    pub message: String,
    pub is_error: bool,
}

pub type EventSender = broadcast::Sender<Event>;

pub fn emit(events: &EventSender, protocol: Protocol, message: impl Into<String>, is_error: bool) {
    // Logs are bounded, observational events. No receiver must not stop a file transfer.
    let _ = events.send(Event {
        at: SystemTime::now(),
        protocol,
        message: message.into(),
        is_error,
    });
}

/// Owns a listener task. Each server must own its connection tasks inside that task.
/// Dropping a handle cancels it; normal callers use shutdown() and observe completion.
pub struct ServerHandle {
    pub protocol: Protocol,
    pub endpoints: Vec<Endpoint>,
    cancel: CancellationToken,
    task: Option<JoinHandle<Result<()>>>,
}

impl ServerHandle {
    pub fn new(
        protocol: Protocol,
        endpoints: Vec<Endpoint>,
        cancel: CancellationToken,
        task: JoinHandle<Result<()>>,
    ) -> Self {
        Self {
            protocol,
            endpoints,
            cancel,
            task: Some(task),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub async fn shutdown(mut self) -> Result<()> {
        self.cancel.cancel();
        let mut task = self.task.take().context("服务已经停止")?;
        match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
            Ok(result) => result.context("服务任务异常退出")?,
            Err(_) => {
                task.abort();
                let _ = task.await;
                bail!("{} 停止超时，已取消服务任务", self.protocol)
            }
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
