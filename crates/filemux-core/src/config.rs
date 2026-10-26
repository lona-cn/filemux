use anyhow::{Context, Result, bail, ensure};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

fn local(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn data_dir() -> PathBuf {
    ProjectDirs::from("dev", "Filemux", "Filemux")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".filemux-data"))
}

pub fn default_config_path() -> PathBuf {
    ProjectDirs::from("dev", "Filemux", "Filemux")
        .map(|dirs| dirs.config_dir().join("filemux.toml"))
        .unwrap_or_else(|| PathBuf::from("filemux.toml"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub http: HttpConfig,
    pub tftp: TftpConfig,
    pub nfs: NfsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            state_dir: data_dir().join("state"),
            http: HttpConfig::default(),
            tftp: TftpConfig::default(),
            nfs: NfsConfig::default(),
        }
    }
}

impl Config {
    /// Relative filesystem paths are relative to the configuration file, not the caller's cwd.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let source = fs::read_to_string(path)
            .with_context(|| format!("无法读取配置文件 {}", path.display()))?;
        let mut config: Self = toml::from_str(&source)
            .with_context(|| format!("配置文件格式有误：{}", path.display()))?;
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let base = absolute.parent().context("配置文件没有父目录")?;
        if config.root.is_relative() {
            config.root = base.join(&config.root);
        }
        if config.state_dir.is_relative() {
            config.state_dir = base.join(&config.state_dir);
        }
        Ok(config)
    }

    /// Replace a complete configuration atomically; a failed write leaves the old file intact.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建配置目录 {}", parent.display()))?;
        let text = toml::to_string_pretty(self).context("无法序列化配置")?;
        let mut file = tempfile::NamedTempFile::new_in(parent).context("无法创建配置临时文件")?;
        file.write_all(text.as_bytes()).context("无法写入配置")?;
        file.as_file().sync_all().context("无法保存配置到磁盘")?;
        file.persist(path)
            .map_err(|e| e.error)
            .with_context(|| format!("无法保存配置文件 {}", path.display()))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(!self.root.as_os_str().is_empty(), "请选择共享目录");
        let metadata = fs::metadata(&self.root)
            .with_context(|| format!("无法访问共享目录 {}", self.root.display()))?;
        ensure!(
            metadata.is_dir(),
            "共享路径不是目录：{}",
            self.root.display()
        );
        ensure!(
            !self.state_dir.as_os_str().is_empty(),
            "持久状态目录不能为空"
        );
        ensure!(
            (1..=4096).contains(&self.http.max_connections),
            "HTTP 并发连接数应为 1–4096"
        );
        ensure!(
            (1..=1024).contains(&self.tftp.max_transfers),
            "TFTP 并发传输数应为 1–1024"
        );
        ensure!(
            (100..=60_000).contains(&self.tftp.timeout_ms),
            "TFTP 超时应为 100–60000 毫秒"
        );
        ensure!(
            (1..=100).contains(&self.tftp.retries),
            "TFTP 重试次数应为 1–100"
        );
        ensure!(
            (8..=65_464).contains(&self.tftp.block_size_limit),
            "TFTP 块大小上限应为 8–65464 字节"
        );
        ensure!(
            self.tftp.max_upload_bytes > 0,
            "TFTP 上传大小上限必须大于零"
        );
        ensure!(
            (1..=1024).contains(&self.nfs.max_connections),
            "NFS 并发连接数应为 1–1024"
        );
        ensure!(
            (1024..=4 * 1024 * 1024).contains(&self.nfs.max_request_bytes),
            "NFS 请求大小上限应为 1024–4194304 字节"
        );
        ensure!(self.nfs.export.starts_with('/'), "NFS 导出名应以 / 开头");
        ensure!(
            !self.nfs.export.contains('\\')
                && !self.nfs.export.split('/').any(|p| p == ".." || p == "."),
            "NFS 导出名不能包含路径跳转"
        );
        if self.http.enabled && self.nfs.enabled && same_listener(self.http.bind, self.nfs.bind) {
            bail!("HTTP 与 NFS 不能监听同一个 TCP 地址");
        }
        if self.tftp.enabled && self.nfs.enabled && same_listener(self.tftp.bind, self.nfs.bind) {
            bail!("TFTP 与 NFS 不能监听同一个 UDP 地址");
        }
        if self.nfs.enabled {
            if let Some(rpcbind) = self.nfs.rpcbind {
                ensure!(
                    !same_listener(rpcbind, self.nfs.bind),
                    "端口映射与 NFS 应使用不同的监听端口"
                );
                if self.http.enabled && same_listener(rpcbind, self.http.bind) {
                    bail!("端口映射与 HTTP 不能监听同一个 TCP 地址");
                }
                if self.tftp.enabled && same_listener(rpcbind, self.tftp.bind) {
                    bail!("端口映射与 TFTP 不能监听同一个 UDP 地址");
                }
            }
        }
        Ok(())
    }
}

fn same_listener(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() != 0
        && a.port() == b.port()
        && (a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub enabled: bool,
    pub bind: SocketAddr,
    pub max_connections: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: local(8080),
            max_connections: 128,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TftpConfig {
    pub enabled: bool,
    pub bind: SocketAddr,
    pub allow_upload: bool,
    pub max_upload_bytes: u64,
    pub max_transfers: usize,
    pub timeout_ms: u64,
    pub retries: u32,
    pub block_size_limit: u16,
}

impl Default for TftpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: local(69),
            allow_upload: false,
            max_upload_bytes: 1024 * 1024 * 1024,
            max_transfers: 32,
            timeout_ms: 1000,
            retries: 5,
            block_size_limit: 1468,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NfsConfig {
    pub enabled: bool,
    pub bind: SocketAddr,
    #[serde(with = "rpcbind_setting")]
    pub rpcbind: Option<SocketAddr>,
    pub export: String,
    pub max_connections: usize,
    pub max_request_bytes: usize,
}

impl Default for NfsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: local(2049),
            rpcbind: Some(local(111)),
            export: "/".into(),
            max_connections: 64,
            max_request_bytes: 1024 * 1024,
        }
    }
}

// TOML has no null. Encode a disabled mapper explicitly so a missing Option
// cannot reload as the struct's enabled-by-default discovery address.
mod rpcbind_setting {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::net::SocketAddr;

    pub fn serialize<S: Serializer>(
        value: &Option<SocketAddr>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(address) => address.serialize(serializer),
            None => serializer.serialize_bool(false),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SocketAddr>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Setting {
            Address(SocketAddr),
            Disabled(bool),
        }
        match Setting::deserialize(deserializer)? {
            Setting::Address(address) => Ok(Some(address)),
            Setting::Disabled(false) => Ok(None),
            Setting::Disabled(true) => Err(serde::de::Error::custom(
                "关闭端口映射请填写 false，启用时请填写 IP:端口",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_paths_against_config_location() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("share")).unwrap();
        let file = dir.path().join("filemux.toml");
        fs::write(&file, "root = 'share'\nstate_dir = 'state'\n").unwrap();
        let config = Config::load(&file).unwrap();
        assert_eq!(config.root, dir.path().join("share"));
        assert_eq!(config.state_dir, dir.path().join("state"));
        config.validate().unwrap();
    }

    #[test]
    fn rejects_unknown_settings_and_invalid_transfer_limits() {
        assert!(toml::from_str::<Config>("[tftp]\nallow_uplaod = true").is_err());
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            root: dir.path().to_path_buf(),
            ..Config::default()
        };
        config.tftp.timeout_ms = 0;
        assert!(config.validate().is_err());
        config.tftp.timeout_ms = 1000;
        config.tftp.max_upload_bytes = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_conflicting_enabled_listeners_but_allows_ephemeral_ports() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            root: dir.path().to_path_buf(),
            ..Config::default()
        };
        config.nfs.enabled = true;
        config.nfs.bind = config.http.bind;
        assert!(config.validate().is_err());
        config.http.bind = local(0);
        config.nfs.bind = local(0);
        config.nfs.rpcbind = Some(local(0));
        config.validate().unwrap();
    }

    #[test]
    fn disabled_rpc_discovery_stays_disabled_after_save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("share");
        fs::create_dir(&root).unwrap();
        let mut config = Config {
            root,
            state_dir: dir.path().join("state"),
            ..Config::default()
        };
        config.nfs.enabled = true;
        config.nfs.rpcbind = None;
        let path = dir.path().join("filemux.toml");
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert!(
            loaded.nfs.rpcbind.is_none(),
            "disabled discovery was unexpectedly restored"
        );
    }
}
