//! Explicit, pinned boot-tool downloads. Downloading never enables a service.

use super::{resource_path, validate_tool_paths};
use crate::Config;
use crate::fs::{NodeKind, SharedFs};
use anyhow::{Context, Result, bail, ensure};
use cap_fs_ext::MetadataExt;
use flate2::read::MultiGzDecoder;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const MAX_DOWNLOAD: u64 = 32 * 1024 * 1024;
const MAX_EXPANDED: u64 = 64 * 1024 * 1024;
const MAX_MEMBER: u64 = 8 * 1024 * 1024;
const IPXE_MEMBERS: [&[u8]; 2] = [
    b"ipxeboot/i386/undionly.kpxe",
    b"ipxeboot/x86_64/snponly.efi",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootTool {
    Ipxe,
    Wimboot,
}

#[derive(Debug)]
pub struct ToolSpec {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub source_url: &'static str,
    pub license_url: &'static str,
}

impl BootTool {
    pub fn spec(self) -> &'static ToolSpec {
        match self {
            Self::Ipxe => &ToolSpec {
                version: "2.0.0",
                url: "https://github.com/ipxe/ipxe/releases/download/v2.0.0/ipxeboot.tar.gz",
                sha256: "01a526d4cc791fc30362259c609d6c506cc64a7bdff51b9a5eb788354e17eee1",
                source_url: "https://github.com/ipxe/ipxe/tree/v2.0.0",
                license_url: "https://ipxe.org/licensing",
            },
            Self::Wimboot => &ToolSpec {
                version: "2.9.0",
                url: "https://github.com/ipxe/wimboot/releases/download/v2.9.0/wimboot",
                sha256: "5f067ccdc4d084d5bf77b6c853bd0f8402dfc2b4cd1b103d358993ae97fae8e3",
                source_url: "https://github.com/ipxe/wimboot/tree/v2.9.0",
                license_url: "https://github.com/ipxe/wimboot/blob/v2.9.0/LICENSE.txt",
            },
        }
    }

    fn paths(self, config: &Config) -> Vec<String> {
        match self {
            Self::Ipxe => vec![
                config.pxe.bios_bootfile.clone(),
                config.pxe.uefi_x64_bootfile.clone(),
            ],
            Self::Wimboot => vec![config.pxe.wimboot.clone()],
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DownloadReport {
    pub saved: Vec<String>,
    /// Existing regular files are preserved, not replaced or revalidated as boot images.
    pub skipped: Vec<String>,
}

/// Downloads only the fixed official catalog; incomplete, disabled PXE settings are allowed.
/// Cancellation is cooperative and awaited through filesystem cleanup before returning.
pub async fn download_tool(
    config: &Config,
    tool: BootTool,
    cancel: CancellationToken,
) -> Result<DownloadReport> {
    let spec = tool.spec();
    download_with_spec(config, tool, spec.url, spec.sha256, cancel).await
}

// Private so neither a configuration file nor a network API can select a download source.
async fn download_with_spec(
    config: &Config,
    tool: BootTool,
    url: &str,
    sha256: &str,
    cancel: CancellationToken,
) -> Result<DownloadReport> {
    let paths = tool.paths(config);
    let config = config.clone();
    let prepare_paths = paths.clone();
    let prepare_cancel = cancel.clone();
    let mut session = tokio::task::spawn_blocking(move || {
        Session::prepare(&config, prepare_paths, &prepare_cancel)
    })
    .await
    .map_err(|error| failure(error.into(), &DownloadReport::default(), &paths, &cancel))?
    .map_err(|error| anyhow::anyhow!("下载来源：{url}\n{error:#}"))?;

    let fetched = if session
        .targets
        .iter()
        .all(|target| target.existing.is_some())
    {
        Ok(None)
    } else {
        fetch(url, sha256, &cancel).await.map(Some)
    };
    let source = url.to_owned();
    // All capability operations, extraction, publication and drop/cleanup run off the worker.
    tokio::task::spawn_blocking(move || {
        let result = fetched.and_then(|file| {
            cancelled(&cancel)?;
            if let Some(file) = file {
                session.publish(tool, file, &cancel)?;
            }
            session.check_existing()?;
            cancelled(&cancel)
        });
        result.map_err(|error| {
            failure(
                error.context(format!("下载来源：{source}")),
                &session.report,
                &paths,
                &cancel,
            )
        })?;
        Ok(session.report)
    })
    .await
    .context("启动文件发布任务异常")?
}

fn cancelled(cancel: &CancellationToken) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "启动工具下载已取消");
    Ok(())
}

fn failure(
    error: anyhow::Error,
    report: &DownloadReport,
    paths: &[String],
    cancel: &CancellationToken,
) -> anyhow::Error {
    let unsaved: Vec<_> = paths
        .iter()
        .filter(|path| !report.saved.contains(path) && !report.skipped.contains(path))
        .map(String::as_str)
        .collect();
    let display = |values: Vec<&str>| {
        if values.is_empty() {
            "（无）".into()
        } else {
            values.join("、")
        }
    };
    anyhow::anyhow!(
        "{error:#}\n已保存：{}；已存在，未覆盖：{}；未保存：{}{}",
        display(report.saved.iter().map(String::as_str).collect()),
        display(report.skipped.iter().map(String::as_str).collect()),
        display(unsaved),
        if cancel.is_cancelled() {
            "；下载已取消"
        } else {
            ""
        },
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    created: SystemTime,
}

impl FileIdentity {
    fn from_metadata(metadata: &cap_std::fs::Metadata) -> Result<Self> {
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            created: metadata.created()?.into_std(),
        })
    }

    fn of_file(file: &File) -> Result<Self> {
        Self::from_metadata(&cap_std::fs::Metadata::from_file(file)?)
    }

    fn of_directory(path: &Path) -> Result<Self> {
        let dir = cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        Self::from_metadata(&dir.dir_metadata()?)
    }
}

struct Existing {
    id: u64,
    identity: FileIdentity,
    // Retaining the handle also prevents a removed object's native ID being recycled.
    _file: File,
}

struct Target {
    path: String,
    parent: PathBuf,
    resolved_parent: PathBuf,
    parent_id: u64,
    existing: Option<Existing>,
}

struct Session {
    fs: SharedFs,
    root_identity: FileIdentity,
    targets: Vec<Target>,
    report: DownloadReport,
}

impl Session {
    fn prepare(config: &Config, paths: Vec<String>, cancel: &CancellationToken) -> Result<Self> {
        let mut report = DownloadReport::default();
        let result = (|| {
            cancelled(cancel)?;
            validate_tool_paths(&config.pxe)?;
            ensure!(!config.root.as_os_str().is_empty(), "请选择共享目录");
            ensure!(
                !config.state_dir.as_os_str().is_empty(),
                "持久状态目录不能为空"
            );
            for path in &paths {
                resource_path(path)?;
            }
            let root = std::fs::canonicalize(&config.root).context("无法解析共享目录")?;
            let root_identity = FileIdentity::of_directory(&root).context("无法访问共享目录")?;
            let fs =
                SharedFs::open(&root, &config.state_dir).context("无法打开共享目录和持久状态")?;
            ensure!(
                FileIdentity::of_directory(&root)? == root_identity,
                "共享目录在检查时被替换"
            );
            let mut targets = Vec::with_capacity(paths.len());
            for path in &paths {
                cancelled(cancel)?;
                let relative = resource_path(path)?;
                let parent = relative.parent().unwrap_or(Path::new("")).to_path_buf();
                let resolved_parent = resolve_parent(&fs, &parent)
                    .with_context(|| format!("目标父目录必须已存在且位于共享目录内：{path}"))?;
                let node = fs.lookup_path(&resolved_parent)?;
                ensure!(
                    node.kind == NodeKind::Directory,
                    "目标父路径不是目录：{path}"
                );
                let existing = existing(&fs, path)?;
                if existing.is_some() {
                    report.skipped.push(path.clone());
                }
                targets.push(Target {
                    path: path.clone(),
                    parent,
                    resolved_parent,
                    parent_id: node.id,
                    existing,
                });
            }
            Ok((fs, root_identity, targets))
        })();
        match result {
            Ok((fs, root_identity, targets)) => Ok(Self {
                fs,
                root_identity,
                targets,
                report,
            }),
            Err(error) => Err(failure(error, &report, &paths, cancel)),
        }
    }

    fn check_parent(&self, target: &Target) -> Result<()> {
        ensure!(
            FileIdentity::of_directory(self.fs.root_path())? == self.root_identity,
            "共享目录已被替换"
        );
        let resolved = resolve_parent(&self.fs, &target.parent)?;
        ensure!(
            resolved == target.resolved_parent,
            "目标父路径已被替换：{}",
            target.path
        );
        let node = self.fs.lookup_path(&resolved)?;
        ensure!(
            node.kind == NodeKind::Directory && node.id == target.parent_id,
            "目标父目录已被替换：{}",
            target.path
        );
        Ok(())
    }

    fn check_existing(&self) -> Result<()> {
        for target in &self.targets {
            self.check_parent(target)?;
            if let Some(expected) = &target.existing {
                let current = existing(&self.fs, &target.path)?
                    .with_context(|| format!("原有启动文件已消失：{}", target.path))?;
                ensure!(
                    current.id == expected.id && current.identity == expected.identity,
                    "原有启动文件已被替换：{}",
                    target.path
                );
            }
        }
        Ok(())
    }

    fn publish(&mut self, tool: BootTool, file: File, cancel: &CancellationToken) -> Result<()> {
        let mut files = match tool {
            BootTool::Ipxe => extract_ipxe(file, cancel)?.map(Some),
            BootTool::Wimboot => [Some(file), None],
        };
        for (index, source) in files.iter_mut().flatten().enumerate() {
            cancelled(cancel)?;
            let target = &self.targets[index];
            self.check_parent(target)?;
            if target.existing.is_some() {
                continue;
            }
            let upload = match self.fs.begin_upload(Path::new(&target.path)) {
                Ok(upload) => upload,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let found = existing(&self.fs, &target.path)?
                        .with_context(|| format!("并发创建的目标随后消失：{}", target.path))?;
                    self.report.skipped.push(target.path.clone());
                    self.targets[index].existing = Some(found);
                    continue;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("无法创建启动文件：{}", target.path));
                }
            };
            let mut writer = upload.writer()?;
            let expected = FileIdentity::of_file(&writer)?;
            source.seek(SeekFrom::Start(0))?;
            copy_cancelled(source, &mut writer, cancel)
                .with_context(|| format!("无法写入启动文件：{}", target.path))?;
            drop(writer);
            self.check_parent(target)?;
            cancelled(cancel)?;
            let committed = upload.commit();
            match committed {
                Ok(()) => self.report.saved.push(target.path.clone()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let found = existing(&self.fs, &target.path)?
                        .with_context(|| format!("并发创建的目标随后消失：{}", target.path))?;
                    self.report.skipped.push(target.path.clone());
                    self.targets[index].existing = Some(found);
                    continue;
                }
                Err(error) => {
                    // commit may have published before a subsequent sync/identity check failed.
                    // Preserve that file and report it accurately; never attempt rollback.
                    if existing(&self.fs, &target.path)
                        .ok()
                        .flatten()
                        .is_some_and(|found| found.identity == expected)
                    {
                        self.report.saved.push(target.path.clone());
                    }
                    return Err(error)
                        .with_context(|| format!("无法发布启动文件：{}", target.path));
                }
            }
            self.check_parent(target)?;
            let published = existing(&self.fs, &target.path)?
                .with_context(|| format!("刚发布的启动文件已消失：{}", target.path))?;
            ensure!(
                published.identity == expected,
                "刚发布的启动文件已被替换：{}",
                target.path
            );
        }
        Ok(())
    }
}

fn resolve_parent(fs: &SharedFs, parent: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(fs.root_path().join(parent))?;
    Ok(canonical
        .strip_prefix(fs.root_path())
        .context("目标父目录位于共享目录之外")?
        .to_path_buf())
}

fn existing(fs: &SharedFs, path: &str) -> Result<Option<Existing>> {
    let node = match fs.lookup_path(Path::new(path)) {
        Ok(node) => node,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("无法检查目标：{path}")),
    };
    ensure!(
        node.kind == NodeKind::File,
        "目标已存在但不是常规文件，未覆盖：{path}"
    );
    let file = fs
        .open_by_id(node.id)
        .with_context(|| format!("目标已被替换或无法读取：{path}"))?;
    let identity = FileIdentity::of_file(&file)?;
    ensure!(
        fs.lookup_path(Path::new(path))?.id == node.id,
        "目标在检查时被替换：{path}"
    );
    Ok(Some(Existing {
        id: node.id,
        identity,
        _file: file,
    }))
}

fn allowed_url(url: &reqwest::Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    #[cfg(test)]
    if url.scheme() == "http" {
        return url
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
    }
    false
}

async fn fetch(url: &str, expected: &str, cancel: &CancellationToken) -> Result<File> {
    cancelled(cancel)?;
    let url = reqwest::Url::parse(url).context("下载地址无效")?;
    ensure!(allowed_url(&url), "启动工具只能从固定 HTTPS 来源下载");
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if !allowed_url(attempt.url())
                || (attempt.url().scheme() != "https"
                    && attempt.previous().iter().any(|url| url.scheme() == "https"))
            {
                attempt.error("拒绝下载重定向到非 HTTPS 来源")
            } else if attempt.previous().len() > 5 {
                attempt.error("启动工具下载重定向超过 5 次")
            } else {
                attempt.follow()
            }
        }))
        .https_only(!cfg!(test))
        .retry(reqwest::retry::never())
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .user_agent(concat!("Filemux/", env!("CARGO_PKG_VERSION")));
    #[cfg(test)]
    let builder = if url.scheme() == "http" {
        builder.no_proxy()
    } else {
        builder
    };
    let client = builder.build().context("无法创建 HTTPS 下载客户端")?;
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("启动工具下载已取消"),
        response = client.get(url).send() => response.context("无法下载启动工具")?,
    };
    let mut response = response
        .error_for_status()
        .context("启动工具下载返回失败状态")?;
    if let Some(length) = response.content_length() {
        ensure!(length <= MAX_DOWNLOAD, "启动工具下载超过 32 MiB 限制");
    }
    let temporary = tokio::task::spawn_blocking(tempfile::tempfile)
        .await
        .context("临时文件任务异常")?
        .context("无法创建下载临时文件")?;
    let mut file = tokio::fs::File::from_std(temporary);
    let result = async {
        let mut digest = Sha256::new();
        let mut total = 0_u64;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("启动工具下载已取消"),
                chunk = response.chunk() => chunk.context("读取启动工具下载内容失败")?,
            };
            let Some(chunk) = chunk else { break };
            total += chunk.len() as u64;
            ensure!(total <= MAX_DOWNLOAD, "启动工具下载超过 32 MiB 限制");
            digest.update(&chunk);
            file.write_all(&chunk)
                .await
                .context("无法写入下载临时文件")?;
            cancelled(cancel)?;
        }
        ensure!(total > 0, "启动工具下载内容为空");
        let actual = format!("{:x}", digest.finalize());
        ensure!(
            actual == expected,
            "启动工具 SHA-256 不符：预期 {expected}，实际 {actual}"
        );
        file.flush().await.context("无法完成下载临时文件")?;
        file.rewind().await?;
        cancelled(cancel)
    }
    .await;
    // into_std waits for any outstanding Tokio blocking write, including on failure.
    let file = file.into_std().await;
    match result {
        Ok(()) => Ok(file),
        Err(error) => {
            tokio::task::spawn_blocking(move || drop(file))
                .await
                .context("下载临时文件清理任务异常")?;
            Err(error)
        }
    }
}

fn copy_cancelled(
    reader: &mut impl Read,
    writer: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<u64> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0;
    loop {
        cancelled(cancel)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(total);
        }
        writer.write_all(&buffer[..count])?;
        total += count as u64;
    }
}

struct ExpandedReader<'a, R> {
    inner: R,
    read: u64,
    cancel: &'a CancellationToken,
}

impl<R: Read> Read for ExpandedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("启动工具下载已取消"));
        }
        let available =
            (MAX_EXPANDED.saturating_sub(self.read) + 1).min(buffer.len() as u64) as usize;
        let count = self.inner.read(&mut buffer[..available])?;
        self.read += count as u64;
        if self.read > MAX_EXPANDED {
            return Err(io::Error::other("iPXE 归档解压超过 64 MiB 限制"));
        }
        Ok(count)
    }
}

fn extract_ipxe(file: File, cancel: &CancellationToken) -> Result<[File; 2]> {
    let reader = ExpandedReader {
        inner: MultiGzDecoder::new(file),
        read: 0,
        cancel,
    };
    let mut archive = tar::Archive::new(reader);
    let mut selected: [Option<File>; 2] = [None, None];
    // Raw iteration avoids GNU/PAX path overrides and sparse-file expansion.
    for entry in archive
        .entries()
        .context("无法读取 iPXE tar 归档")?
        .raw(true)
    {
        cancelled(cancel)?;
        let mut entry = entry.context("iPXE tar 成员无效")?;
        let index = IPXE_MEMBERS
            .iter()
            .position(|path| *path == entry.path_bytes().as_ref());
        if let Some(index) = index {
            ensure!(
                entry.header().entry_type().is_file(),
                "iPXE 必需成员不是常规文件：{}",
                String::from_utf8_lossy(IPXE_MEMBERS[index])
            );
            ensure!(selected[index].is_none(), "iPXE 归档包含重复的必需成员");
            let size = entry.header().size()?;
            ensure!(
                size > 0 && size <= MAX_MEMBER,
                "iPXE 必需成员必须非空且不超过 8 MiB"
            );
            let mut output = tempfile::tempfile().context("无法创建 iPXE 解压临时文件")?;
            let copied = copy_cancelled(&mut entry, &mut output, cancel)?;
            ensure!(copied == size, "iPXE 必需成员被截断");
            output.seek(SeekFrom::Start(0))?;
            selected[index] = Some(output);
        } else {
            // Unknown paths, traversal names and links are only bytes to discard. Never unpack.
            copy_cancelled(&mut entry, &mut io::sink(), cancel)?;
        }
    }
    // tar stops at its end marker; drain the gzip stream to enforce the total expansion
    // limit and checksum even for trailing data or additional gzip members.
    copy_cancelled(&mut archive.into_inner(), &mut io::sink(), cancel)?;
    let [bios, uefi] = selected;
    Ok([
        bios.context("iPXE 归档缺少 ipxeboot/i386/undionly.kpxe")?,
        uefi.context("iPXE 归档缺少 ipxeboot/x86_64/snponly.efi")?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Cursor;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    fn fixture() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("share");
        std::fs::create_dir(&root).unwrap();
        let config = Config {
            root,
            state_dir: dir.path().join("state"),
            ..Config::default()
        };
        (dir, config)
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn assert_files(config: &Config, expected: &[&str]) {
        let mut actual: Vec<_> = std::fs::read_dir(&config.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        actual.sort();
        let mut expected: Vec<_> = expected.iter().map(|path| path.to_string()).collect();
        expected.sort();
        assert_eq!(actual, expected);
    }

    // A real, one-request HTTP endpoint with independently encoded chunk framing.
    async fn serve(bytes: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/tool", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            for chunk in bytes.chunks(7) {
                socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(chunk).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
        });
        (url, task)
    }

    #[tokio::test]
    async fn downloads_verified_bytes_and_preserves_existing_targets_without_network() {
        let (_dir, mut config) = fixture();
        // Unrelated incomplete PXE/service settings must not prevent downloading.
        config.pxe.enabled = true;
        config.pxe.bios_bootfile = "../not-yet-configured".into();
        config.http.max_connections = 0;
        let bytes = b"real loopback download\0with binary bytes".to_vec();
        let (url, server) = serve(bytes.clone()).await;
        let report = download_with_spec(
            &config,
            BootTool::Wimboot,
            &url,
            &digest(&bytes),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(report.saved, ["wimboot"]);
        assert!(report.skipped.is_empty());
        assert_eq!(std::fs::read(config.root.join("wimboot")).unwrap(), bytes);
        let report = download_with_spec(
            &config,
            BootTool::Wimboot,
            "not a URL",
            "",
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(report.saved.is_empty());
        assert_eq!(report.skipped, ["wimboot"]);
        assert_eq!(std::fs::read(config.root.join("wimboot")).unwrap(), bytes);
        assert_files(&config, &["wimboot"]);
    }

    #[tokio::test]
    async fn digest_and_transport_failures_publish_nothing_and_clean_up() {
        let (_dir, config) = fixture();
        let (url, server) = serve(b"wrong digest".to_vec()).await;
        let error = download_with_spec(
            &config,
            BootTool::Wimboot,
            &url,
            &"0".repeat(64),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        let message = format!("{error:#}");
        assert!(
            message.contains("SHA-256") && message.contains("未保存：wimboot"),
            "{message}"
        );
        assert_files(&config, &[]);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/truncated", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\npartial",
                )
                .await
                .unwrap();
        });
        assert!(
            download_with_spec(
                &config,
                BootTool::Wimboot,
                &url,
                "",
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        server.await.unwrap();
        assert_files(&config, &[]);
    }

    #[tokio::test]
    async fn cancellation_waits_for_cleanup_and_reports_unpublished_paths() {
        let (_dir, config) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/slow", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\npartial")
                .await
                .unwrap();
            ready_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_config = config.clone();
        let task = tokio::spawn(async move {
            download_with_spec(&task_config, BootTool::Wimboot, &url, "", task_cancel).await
        });
        ready_rx.await.unwrap();
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("取消") && message.contains("未保存：wimboot"),
            "{message}"
        );
        assert_files(&config, &[]);
        server.abort();
        let _ = server.await;
    }

    fn tar_member(
        builder: &mut tar::Builder<Vec<u8>>,
        name: &[u8],
        kind: tar::EntryType,
        bytes: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        // Raw header bytes deliberately permit a traversal name that Builder::append_data
        // would refuse to create. The downloader must never use it as a filesystem path.
        header.as_mut_bytes()[..name.len()].copy_from_slice(name);
        header.set_entry_type(kind);
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        if kind.is_symlink() {
            header.set_link_name("ipxeboot/i386/undionly.kpxe").unwrap();
        }
        header.set_cksum();
        builder.append(&header, Cursor::new(bytes)).unwrap();
    }

    fn archive(link_instead_of_bios: bool) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        tar_member(
            &mut builder,
            b"../outside",
            tar::EntryType::Regular,
            b"not a target",
        );
        tar_member(&mut builder, b"ignored-link", tar::EntryType::Symlink, b"");
        tar_member(
            &mut builder,
            IPXE_MEMBERS[0],
            if link_instead_of_bios {
                tar::EntryType::Symlink
            } else {
                tar::EntryType::Regular
            },
            if link_instead_of_bios {
                b""
            } else {
                b"bios binary"
            },
        );
        tar_member(
            &mut builder,
            IPXE_MEMBERS[1],
            tar::EntryType::Regular,
            b"uefi binary",
        );
        let tar = builder.into_inner().unwrap();
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&tar).unwrap();
        gzip.finish().unwrap()
    }

    #[tokio::test]
    async fn extracts_only_exact_regular_members_and_rejects_expected_links() {
        let (dir, config) = fixture();
        let bytes = archive(false);
        let (url, server) = serve(bytes.clone()).await;
        let report = download_with_spec(
            &config,
            BootTool::Ipxe,
            &url,
            &digest(&bytes),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(report.saved, ["undionly.kpxe", "snponly.efi"]);
        assert_eq!(
            std::fs::read(config.root.join("undionly.kpxe")).unwrap(),
            b"bios binary"
        );
        assert_eq!(
            std::fs::read(config.root.join("snponly.efi")).unwrap(),
            b"uefi binary"
        );
        assert!(!dir.path().join("outside").exists());
        assert_files(&config, &["undionly.kpxe", "snponly.efi"]);

        let (_dir, config) = fixture();
        std::fs::write(config.root.join("undionly.kpxe"), b"local BIOS tool").unwrap();
        let bytes = archive(false);
        let (url, server) = serve(bytes.clone()).await;
        let report = download_with_spec(
            &config,
            BootTool::Ipxe,
            &url,
            &digest(&bytes),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(report.saved, ["snponly.efi"]);
        assert_eq!(report.skipped, ["undionly.kpxe"]);
        assert_eq!(
            std::fs::read(config.root.join("undionly.kpxe")).unwrap(),
            b"local BIOS tool"
        );
        assert_eq!(
            std::fs::read(config.root.join("snponly.efi")).unwrap(),
            b"uefi binary"
        );
        assert_files(&config, &["undionly.kpxe", "snponly.efi"]);

        let (_dir, config) = fixture();
        let bytes = archive(true);
        let (url, server) = serve(bytes.clone()).await;
        let error = download_with_spec(
            &config,
            BootTool::Ipxe,
            &url,
            &digest(&bytes),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        assert!(format!("{error:#}").contains("不是常规文件"));
        assert_files(&config, &[]);
    }

    #[tokio::test]
    async fn detects_replaced_parent_and_reports_independently_published_file() {
        let (_dir, mut config) = fixture();
        std::fs::create_dir(config.root.join("efi")).unwrap();
        config.pxe.uefi_x64_bootfile = "efi/snponly.efi".into();
        let bytes = archive(false);
        let expected = digest(&bytes);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let (send_tx, send_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            socket.read(&mut request).await.unwrap();
            accepted_tx.send(()).unwrap();
            send_rx.await.unwrap();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&bytes).await.unwrap();
        });
        let task_config = config.clone();
        let task = tokio::spawn(async move {
            download_with_spec(
                &task_config,
                BootTool::Ipxe,
                &url,
                &expected,
                CancellationToken::new(),
            )
            .await
        });
        accepted_rx.await.unwrap(); // preflight has completed before the HTTP request.
        std::fs::rename(config.root.join("efi"), config.root.join("retired")).unwrap();
        std::fs::create_dir(config.root.join("efi")).unwrap();
        send_tx.send(()).unwrap();
        let error = task.await.unwrap().unwrap_err();
        server.await.unwrap();
        let message = format!("{error:#}");
        assert!(
            message.contains("被替换")
                && message.contains("已保存：undionly.kpxe")
                && message.contains("未保存：efi/snponly.efi"),
            "{message}"
        );
        assert_eq!(
            std::fs::read(config.root.join("undionly.kpxe")).unwrap(),
            b"bios binary"
        );
        assert!(!config.root.join("efi/snponly.efi").exists());
        assert_files(&config, &["efi", "retired", "undionly.kpxe"]);
    }

    #[tokio::test]
    async fn rejects_case_collisions_nonregular_targets_and_missing_parents_without_network() {
        let (_dir, mut config) = fixture();
        config.pxe.uefi_x64_bootfile = "WIMBOOT".into();
        let error = download_with_spec(
            &config,
            BootTool::Wimboot,
            "not a URL",
            "",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("互不相同"));
        assert!(!config.state_dir.exists());
        config.pxe.uefi_x64_bootfile = "snponly.efi".into();
        std::fs::create_dir(config.root.join("wimboot")).unwrap();
        let error = download_with_spec(
            &config,
            BootTool::Wimboot,
            "not a URL",
            "",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("不是常规文件"));
        config.pxe.wimboot = "missing/wimboot".into();
        let error = download_with_spec(
            &config,
            BootTool::Wimboot,
            "not a URL",
            "",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("父目录必须已存在"));
        assert_files(&config, &["wimboot"]);
    }
}
