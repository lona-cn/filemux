//! PXE resource inspection and the prepared iPXE menu. Inspection is not a boot test.

pub mod download;
pub mod proxy;

use crate::fs::SharedFs;
use crate::{Config, PxeConfig, PxeEntry, PxeMode};
use anyhow::{Context, Result, ensure};
use axum::body::Bytes;
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use std::collections::HashSet;
use std::fmt::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

pub(crate) const MENU_PATH: &str = "/.filemux-pxe/menu.ipxe";
const URL_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Clone)]
pub struct PreparedPxe {
    pub(crate) menu: Bytes,
}

#[derive(Clone, Debug)]
pub struct PxeCheckReport {
    pub menu_url: String,
    pub resources: Vec<PxeResourceCheck>,
    pub warnings: Vec<String>,
}

impl PxeCheckReport {
    pub fn is_ok(&self) -> bool {
        self.resources
            .iter()
            .all(|resource| resource.error.is_none())
    }
}

#[derive(Clone, Debug)]
pub struct PxeResourceCheck {
    pub name: String,
    pub path: String,
    pub error: Option<String>,
}

pub fn menu_url(config: &Config) -> String {
    format!(
        "http://{}:{}{MENU_PATH}",
        config.pxe.server_ip,
        config.http.bind.port()
    )
}

pub(crate) fn resource_path(value: &str) -> Result<PathBuf> {
    ensure!(!value.is_empty(), "资源路径不能为空");
    ensure!(
        !value.contains(['\\', ':'])
            && !value.chars().any(char::is_control)
            && value
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "资源路径必须是使用 / 的共享目录相对路径：{value}"
    );
    crate::fs::validate_path(Path::new(value))
        .with_context(|| format!("资源路径无效或使用了保留名称：{value}"))
}

pub(crate) fn validate_tool_paths(config: &PxeConfig) -> Result<()> {
    let paths = [
        &config.bios_bootfile,
        &config.uefi_x64_bootfile,
        &config.wimboot,
    ];
    for (index, path) in paths.iter().enumerate() {
        for other in &paths[..index] {
            ensure!(
                !path.eq_ignore_ascii_case(other),
                "BIOS、UEFI 与 wimboot 的目标路径必须互不相同：{path}"
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_config(config: &Config) -> Result<()> {
    let pxe = &config.pxe;
    ensure!(
        !pxe.server_ip.is_unspecified()
            && !pxe.server_ip.is_multicast()
            && !pxe.server_ip.is_broadcast(),
        "PXE 服务 IPv4 必须是明确的本机单播地址"
    );
    ensure!(
        config.http.bind.port() != 0,
        "PXE 的 HTTP 必须使用非零固定端口"
    );
    for (name, bind) in [("HTTP", config.http.bind), ("TFTP", config.tftp.bind)] {
        ensure!(
            matches!(bind.ip(), IpAddr::V4(ip) if ip.is_unspecified() || ip == pxe.server_ip),
            "PXE 的 {name} 必须监听 {} 或 0.0.0.0",
            pxe.server_ip
        );
    }
    ensure!(
        config.tftp.bind.port() == 69,
        "PXE 的 TFTP 必须监听标准端口 69"
    );
    validate_tool_paths(pxe)?;
    for path in [&pxe.bios_bootfile, &pxe.uefi_x64_bootfile] {
        resource_path(path)?;
        ensure!(
            path.len() <= 127,
            "启动文件名的 UTF-8 长度不能超过 127 字节：{path}"
        );
    }
    ensure!(!pxe.entries.is_empty(), "启用 PXE 时至少需要一个菜单项");
    let mut ids = HashSet::new();
    for entry in &pxe.entries {
        let id = entry.id();
        ensure!(
            !id.is_empty()
                && id.len() <= 32
                && id.as_bytes()[0].is_ascii_alphabetic()
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
            "PXE 菜单 ID 必须匹配 [A-Za-z][A-Za-z0-9_-]{{0,31}}：{id}"
        );
        ensure!(ids.insert(id), "PXE 菜单 ID 重复：{id}");
        ensure!(
            !entry.title().trim().is_empty() && !entry.title().chars().any(char::is_control),
            "PXE 菜单标题不能为空或含控制字符：{id}"
        );
        match entry {
            PxeEntry::Linux {
                kernel,
                initrd,
                args,
                ..
            } => {
                resource_path(kernel)?;
                resource_path(initrd)?;
                ensure!(
                    !args.chars().any(char::is_control),
                    "Linux 参数不能含控制字符：{id}"
                );
            }
            PxeEntry::Winpe { boot_wim, .. } => {
                resource_path(boot_wim)?;
                resource_path(&pxe.wimboot)?;
            }
            PxeEntry::Chain { script, .. } => {
                resource_path(script)?;
            }
        }
    }
    if let Some(default) = &pxe.default_entry {
        ensure!(
            ids.contains(default.as_str()),
            "默认菜单 ID 不存在：{default}"
        );
    }
    Ok(())
}

pub(crate) fn selected_interface(ip: Ipv4Addr, require_netmask: bool) -> Result<NetworkInterface> {
    let interface = NetworkInterface::show()
        .context("无法读取本机网络接口")?
        .into_iter()
        .find(|interface| {
            interface
                .addr
                .iter()
                .any(|address| address.ip() == IpAddr::V4(ip))
        })
        .with_context(|| format!("PXE IPv4 不属于当前本机接口：{ip}"))?;
    ensure!(
        interface.index != 0,
        "PXE 接口索引无效：{} ({ip})",
        interface.name
    );
    if require_netmask {
        let mask = interface
            .addr
            .iter()
            .find(|address| address.ip() == IpAddr::V4(ip))
            .and_then(|address| address.netmask());
        ensure!(
            matches!(mask, Some(IpAddr::V4(mask)) if valid_netmask(mask)),
            "ProxyDHCP 接口缺少有效 IPv4 netmask：{} ({ip})",
            interface.name
        );
    }
    Ok(interface)
}

fn valid_netmask(mask: Ipv4Addr) -> bool {
    let inverted = !u32::from(mask);
    u32::from(mask) != 0 && inverted & inverted.wrapping_add(1) == 0
}

pub(crate) fn required_resources(config: &PxeConfig) -> Vec<(String, &str)> {
    let mut resources = vec![
        ("BIOS iPXE".into(), config.bios_bootfile.as_str()),
        ("UEFI x64 iPXE".into(), config.uefi_x64_bootfile.as_str()),
    ];
    if config
        .entries
        .iter()
        .any(|entry| matches!(entry, PxeEntry::Winpe { .. }))
    {
        resources.push(("wimboot".into(), config.wimboot.as_str()));
    }
    for entry in &config.entries {
        match entry {
            PxeEntry::Linux {
                id, kernel, initrd, ..
            } => {
                resources.push((format!("{id}: kernel"), kernel));
                resources.push((format!("{id}: initrd"), initrd));
            }
            PxeEntry::Winpe { id, boot_wim, .. } => {
                resources.push((format!("{id}: boot.wim"), boot_wim))
            }
            PxeEntry::Chain { id, script, .. } => resources.push((format!("{id}: iPXE"), script)),
        }
    }
    resources
}

/// Never opens SharedFs, creates state, binds a socket, or accesses the network.
pub fn check_resources(config: &Config) -> Result<PxeCheckReport> {
    config.validate()?;
    validate_config(config)?;
    selected_interface(config.pxe.server_ip, config.pxe.mode == PxeMode::ProxyDhcp)?;
    let root = std::fs::canonicalize(&config.root).context("无法解析共享目录")?;
    let resources = required_resources(&config.pxe)
        .into_iter()
        .map(|(name, path)| {
            let error = (|| -> Result<()> {
                let relative = resource_path(path)?;
                let full = std::fs::canonicalize(root.join(relative))
                    .with_context(|| format!("无法访问资源 {path}"))?;
                ensure!(full.starts_with(&root), "资源位于共享目录之外：{path}");
                let metadata = std::fs::metadata(&full)?;
                ensure!(
                    metadata.is_file() && metadata.len() > 0,
                    "资源必须是非空常规文件：{path}"
                );
                Ok(())
            })()
            .err()
            .map(|error| format!("{error:#}"));
            PxeResourceCheck {
                name,
                path: path.into(),
                error,
            }
        })
        .collect();
    let mut warnings = vec!["仅检查路径和非空常规文件，不证明镜像可启动；文件随后仍可能被移动或修改。".into(), "需要独立 DHCP 分配地址；仅支持同网段 BIOS x86 / UEFI x64，不支持 relay、IPv6 或 Secure Boot。".into()];
    if config.pxe.server_ip.is_loopback() {
        warnings.push("当前 IPv4 是 loopback，仅供本机软件验证，其他机器无法访问。".into());
    }
    Ok(PxeCheckReport {
        menu_url: menu_url(config),
        resources,
        warnings,
    })
}

/// Run in a blocking task: inspect every resource, then use the same rooted opens
/// as HTTP and TFTP. Only the menu is retained; boot images are never cached.
pub(crate) fn prepare(config: &Config, fs: &SharedFs) -> Result<PreparedPxe> {
    let report = check_resources(config)?;
    let errors = report
        .resources
        .iter()
        .filter_map(|resource| {
            resource
                .error
                .as_ref()
                .map(|error| format!("{}：{error}", resource.path))
        })
        .collect::<Vec<_>>();
    ensure!(
        errors.is_empty(),
        "PXE 启动资源检查失败：\n{}",
        errors.join("\n")
    );
    for resource in &report.resources {
        let file = fs
            .open_read(&resource_path(&resource.path)?)
            .with_context(|| format!("无法通过共享文件服务读取 PXE 资源：{}", resource.path))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("无法读取 PXE 资源的文件信息：{}", resource.path))?;
        ensure!(
            metadata.len() > 0,
            "PXE 资源已变为空文件：{}",
            resource.path
        );
    }
    Ok(PreparedPxe {
        menu: Bytes::from(render_menu(config)),
    })
}

fn resource_url(config: &Config, path: &str) -> String {
    let mut url = format!(
        "http://{}:{}",
        config.pxe.server_ip,
        config.http.bind.port()
    );
    for component in path.split('/') {
        url.push('/');
        write!(url, "{}", utf8_percent_encode(component, URL_COMPONENT)).unwrap();
    }
    url
}

fn hex_setting(script: &mut String, name: &str, id: &str, value: &str) {
    write!(script, "set {name}-{id}:hex ").unwrap();
    for (index, byte) in value.bytes().enumerate() {
        if index != 0 {
            script.push(':');
        }
        write!(script, "{byte:02x}").unwrap();
    }
    script.push('\n');
}

fn render_menu(config: &Config) -> String {
    let pxe = &config.pxe;
    // prepare() has checked that there is an entry and that the default exists.
    let default = pxe
        .default_entry
        .as_deref()
        .unwrap_or_else(|| pxe.entries[0].id());
    let mut script = String::from("#!ipxe\n");
    writeln!(
        script,
        "set fm-timeout {}",
        u32::from(pxe.timeout_secs) * 1000
    )
    .unwrap();
    for entry in &pxe.entries {
        let id = entry.id();
        // iPXE finds command separators before expanding settings. Hex-encoded
        // UTF-8 keeps titles and arguments out of its command parser.
        hex_setting(&mut script, "fm-title", id, entry.title());
        if let PxeEntry::Linux { args, .. } = entry
            && !args.is_empty()
        {
            hex_setting(&mut script, "fm-args", id, args);
        }
    }
    script.push_str("\n:fm-menu\nmenu Filemux PXE\n");
    for entry in &pxe.entries {
        let id = entry.id();
        writeln!(script, "item fm-entry-{id} ${{fm-title-{id}:string}}").unwrap();
    }
    writeln!(
        script,
        "choose --default fm-entry-{default} --timeout ${{fm-timeout}} fm-selected || exit"
    )
    .unwrap();
    script.push_str("goto ${fm-selected}\n");
    for entry in &pxe.entries {
        let id = entry.id();
        writeln!(script, "\n:fm-entry-{id}").unwrap();
        match entry {
            PxeEntry::Linux {
                kernel,
                initrd,
                args,
                ..
            } => {
                write!(script, "kernel {}", resource_url(config, kernel)).unwrap();
                if !args.is_empty() {
                    write!(script, " ${{fm-args-{id}:string}}").unwrap();
                }
                script.push_str(" || goto fm-failed\n");
                writeln!(
                    script,
                    "initrd {} || goto fm-failed",
                    resource_url(config, initrd)
                )
                .unwrap();
                script.push_str("boot || goto fm-failed\n");
            }
            PxeEntry::Winpe { boot_wim, .. } => {
                writeln!(
                    script,
                    "kernel {} || goto fm-failed",
                    resource_url(config, &pxe.wimboot)
                )
                .unwrap();
                writeln!(
                    script,
                    "initrd {} boot.wim || goto fm-failed",
                    resource_url(config, boot_wim)
                )
                .unwrap();
                script.push_str("boot || goto fm-failed\n");
            }
            PxeEntry::Chain { script: chain, .. } => {
                writeln!(
                    script,
                    "chain {} || goto fm-failed",
                    resource_url(config, chain)
                )
                .unwrap();
            }
        }
        // A boot or chained script may return successfully without booting.
        script.push_str("goto fm-failed\n");
    }
    script.push_str("\n:fm-failed\nset fm-timeout 0\nimgfree\necho Boot failed or returned. Please choose a boot entry.\ngoto fm-menu\n");
    script
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::{Client, StatusCode, header};
    use std::sync::Arc;
    use std::time::Duration;

    fn write_resources(config: &Config) {
        for (_, path) in required_resources(&config.pxe) {
            let full = config.root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, b"fixture, not a boot image").unwrap();
        }
    }

    fn client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    async fn start_menu(
        config: &mut Config,
    ) -> (
        crate::ServerHandle,
        tokio::sync::broadcast::Receiver<crate::Event>,
    ) {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        config.http.bind = reservation.local_addr().unwrap();
        let fs = Arc::new(SharedFs::open(&config.root, &config.state_dir).unwrap());
        let pxe = prepare(config, &fs).unwrap();
        let (events, receiver) = tokio::sync::broadcast::channel(64);
        drop(reservation);
        let server = crate::http::start(fs, config.http.clone(), Some(pxe), events)
            .await
            .unwrap();
        (server, receiver)
    }

    #[tokio::test]
    async fn pxe_menu_serves_three_types_and_protects_its_reserved_route() {
        let (_dir, mut config) = fixture();
        let title = "Linux ; ${danger} 中文";
        let args = "initrd=initrd.magic console=ttyS0; ${danger} 中文";
        config.pxe.entries = vec![
            PxeEntry::Linux {
                id: "menu".into(),
                title: title.into(),
                kernel: "linux/内核 file".into(),
                initrd: "linux/init ram.img".into(),
                args: args.into(),
            },
            PxeEntry::Winpe {
                id: "windows".into(),
                title: "Windows PE".into(),
                boot_wim: "win/启动 文件.wim".into(),
            },
            PxeEntry::Chain {
                id: "failed".into(),
                title: "Custom iPXE".into(),
                script: "scripts/自 定义.ipxe".into(),
            },
        ];
        config.pxe.wimboot = "tools/wim boot".into();
        config.pxe.default_entry = Some("windows".into());
        config.http.allow_upload = true;
        write_resources(&config);
        let (server, mut events) = start_menu(&mut config).await;
        let base = format!("http://{}", server.endpoints[0].address);
        let url = menu_url(&config);
        let client = client();
        let response = client
            .get(&url)
            .header(header::HOST, "untrusted.invalid:9999")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let length = response.content_length().unwrap();
        let script = response.text().await.unwrap();
        assert_eq!(length, script.len() as u64);
        assert!(script.starts_with("#!ipxe\nset fm-timeout 10000\n"));
        assert!(script.contains(
            "choose --default fm-entry-windows --timeout ${fm-timeout} fm-selected || exit\n"
        ));
        assert!(
            script.find("item fm-entry-menu ").unwrap()
                < script.find("item fm-entry-windows ").unwrap()
        );
        assert!(
            script.find("item fm-entry-windows ").unwrap()
                < script.find("item fm-entry-failed ").unwrap()
        );
        for (name, value) in [("fm-title-menu", title), ("fm-args-menu", args)] {
            let hex = value
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(":");
            assert!(script.contains(&format!("set {name}:hex {hex}\n")));
            assert!(!script.contains(value));
        }
        assert!(script.contains("item fm-entry-menu ${fm-title-menu:string}\n"));
        assert!(!script.contains("untrusted.invalid"));
        let kernel = format!("{base}/linux/%E5%86%85%E6%A0%B8%20file");
        let initrd = format!("{base}/linux/init%20ram.img");
        let wimboot = format!("{base}/tools/wim%20boot");
        let wim = format!("{base}/win/%E5%90%AF%E5%8A%A8%20%E6%96%87%E4%BB%B6.wim");
        let chain = format!("{base}/scripts/%E8%87%AA%20%E5%AE%9A%E4%B9%89.ipxe");
        assert!(script.contains(&format!(
            "kernel {kernel} ${{fm-args-menu:string}} || goto fm-failed\n"
        )));
        assert!(script.contains(&format!("initrd {initrd} || goto fm-failed\n")));
        assert!(script.contains(&format!("kernel {wimboot} || goto fm-failed\n")));
        assert!(script.contains(&format!("initrd {wim} boot.wim || goto fm-failed\n")));
        assert!(script.contains(&format!(
            "chain {chain} || goto fm-failed\ngoto fm-failed\n"
        )));
        assert!(script.contains("boot || goto fm-failed\ngoto fm-failed\n"));
        assert!(script.contains(":fm-failed\nset fm-timeout 0\nimgfree\necho "));
        assert!(script.ends_with("goto fm-menu\n"));
        for resource in [&kernel, &initrd, &wimboot, &wim, &chain] {
            let response = client.get(resource.as_str()).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.bytes().await.unwrap().as_ref(),
                b"fixture, not a boot image"
            );
        }
        let response = client.head(&url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            length.to_string()
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(response.bytes().await.unwrap().is_empty());
        for method in [
            reqwest::Method::PUT,
            reqwest::Method::POST,
            reqwest::Method::DELETE,
        ] {
            let response = client
                .request(method, &url)
                .body("replace menu")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[header::ALLOW], "GET, HEAD");
        }
        assert_eq!(
            client.get(&url).send().await.unwrap().text().await.unwrap(),
            script
        );
        let mut logged_get = false;
        let mut logged_put = false;
        while let Ok(event) = events.try_recv() {
            logged_get |= event.message.contains("GET /.filemux-pxe/menu.ipxe: 200");
            logged_put |= event.message.contains("PUT /.filemux-pxe/menu.ipxe: 405");
        }
        assert!(logged_get && logged_put);
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pxe_menu_defaults_to_first_entry_and_zero_waits_for_selection() {
        let (_dir, mut config) = fixture();
        config.pxe.entries.insert(
            0,
            PxeEntry::Linux {
                id: "first".into(),
                title: "First".into(),
                kernel: "vmlinuz".into(),
                initrd: "initrd".into(),
                args: String::new(),
            },
        );
        config.pxe.timeout_secs = 0;
        write_resources(&config);
        let (server, _) = start_menu(&mut config).await;
        let script = client()
            .get(menu_url(&config))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(script.starts_with("#!ipxe\nset fm-timeout 0\n"));
        assert!(script.contains(
            "choose --default fm-entry-first --timeout ${fm-timeout} fm-selected || exit\n"
        ));
        assert!(script.contains(&format!(
            "kernel http://{}/vmlinuz || goto fm-failed\n",
            config.http.bind
        )));
        assert!(!script.contains("fm-args-first"));
        assert!(!script.contains("initrd="));
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pxe_menu_is_not_found_when_disabled_even_with_uploads_enabled() {
        let (_dir, mut config) = fixture();
        config.pxe.enabled = false;
        config.http.bind = "127.0.0.1:0".parse().unwrap();
        config.http.allow_upload = true;
        let fs = Arc::new(SharedFs::open(&config.root, &config.state_dir).unwrap());
        let (events, _) = tokio::sync::broadcast::channel(16);
        let server = crate::http::start(fs, config.http, None, events)
            .await
            .unwrap();
        let url = format!("http://{}{MENU_PATH}", server.endpoints[0].address);
        let client = client();
        for method in [
            reqwest::Method::GET,
            reqwest::Method::HEAD,
            reqwest::Method::PUT,
        ] {
            let head = method == reqwest::Method::HEAD;
            let response = client.request(method, &url).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            if head {
                assert!(response.bytes().await.unwrap().is_empty());
            }
        }
        assert!(!config.root.join(".filemux-pxe").exists());
        server.shutdown().await.unwrap();
    }

    fn fixture() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("share");
        std::fs::create_dir(&root).unwrap();
        let mut config = Config {
            root,
            state_dir: dir.path().join("state"),
            ..Config::default()
        };
        config.pxe.enabled = true;
        config.pxe.server_ip = Ipv4Addr::LOCALHOST;
        config.pxe.entries.push(PxeEntry::Chain {
            id: "local".into(),
            title: "Local".into(),
            script: "boot.ipxe".into(),
        });
        (dir, config)
    }

    #[test]
    fn inspection_collects_missing_resources_without_creating_state() {
        let (_dir, config) = fixture();
        let report = check_resources(&config).unwrap();
        assert_eq!(
            report
                .resources
                .iter()
                .filter(|r| r.error.is_some())
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            ["undionly.kpxe", "snponly.efi", "boot.ipxe"]
        );
        assert!(!report.is_ok());
        assert!(!config.state_dir.exists());
    }

    #[test]
    fn rejects_escaping_paths_duplicate_ids_and_unknown_default() {
        let (_dir, mut config) = fixture();
        for path in [
            "../secret",
            "a/./b",
            "/absolute",
            "C:/boot",
            "a\\b",
            ".filemux-state",
            "a//b",
        ] {
            assert!(resource_path(path).is_err(), "{path}");
        }
        config.pxe.entries.push(config.pxe.entries[0].clone());
        assert!(config.validate().is_err());
        config.pxe.entries.pop();
        config.pxe.default_entry = Some("missing".into());
        assert!(config.validate().is_err());
        config.pxe.enabled = false;
        config.validate().unwrap();
        #[cfg(unix)]
        {
            let outside = config.root.parent().unwrap().join("outside.ipxe");
            std::fs::write(&outside, b"must remain outside the export").unwrap();
            std::os::unix::fs::symlink(&outside, config.root.join("boot.ipxe")).unwrap();
            config.pxe.default_entry = None;
            config.pxe.enabled = true;
            let report = check_resources(&config).unwrap();
            assert!(
                report
                    .resources
                    .iter()
                    .find(|resource| resource.path == "boot.ipxe")
                    .unwrap()
                    .error
                    .is_some()
            );
            assert!(!config.state_dir.exists());
        }
    }
}
