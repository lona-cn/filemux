use crate::i18n::{Language, UiNotice, UiText};
use anyhow::{Context, Result};
use eframe::egui;
use filemux_core::engine::{Engine, ServiceSnapshot};
use filemux_core::pxe::{
    PxeCheckReport,
    download::{BootTool, DownloadReport},
};
use filemux_core::{Config, Event, Protocol, ServiceState};
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const MAX_LOGS: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryTarget {
    Root,
    State,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceOperation {
    Start(Protocol),
    Stop(Protocol),
    StopAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PxeTask {
    Check,
    Download(BootTool),
}

enum PxeResult {
    Check(std::result::Result<PxeCheckReport, String>),
    Download(std::result::Result<DownloadReport, String>),
}

pub enum Command {
    Save(Config),
    Start(Protocol),
    Stop(Protocol),
    StopAll,
    PickDirectory(DirectoryTarget, PathBuf, Language),
    CheckPxe(Config),
    DownloadBootTool(BootTool),
}

impl Command {
    fn operation(&self) -> Option<ServiceOperation> {
        match self {
            Self::Start(protocol) => Some(ServiceOperation::Start(*protocol)),
            Self::Stop(protocol) => Some(ServiceOperation::Stop(*protocol)),
            Self::StopAll => Some(ServiceOperation::StopAll),
            _ => None,
        }
    }
}

pub struct UiState {
    pub snapshots: Option<Vec<ServiceSnapshot>>,
    pub active_config: Option<Config>,
    pub operation: Option<ServiceOperation>,
    pub saving: bool,
    pub picking: Option<DirectoryTarget>,
    pub notice: Option<UiNotice>,
    pub saved: Option<Config>,
    pub picked: Option<(DirectoryTarget, PathBuf)>,
    pub logs: VecDeque<Event>,
    pub service_errors: Vec<(Protocol, UiNotice)>,
    pub settings_in_use: bool,
    pub stopped: bool,
    pub pxe_task: Option<PxeTask>,
    pub pxe_check: Option<std::result::Result<PxeCheckReport, String>>,
    pub pxe_download: Option<std::result::Result<DownloadReport, String>>,
}

pub struct Worker {
    pub state: Arc<Mutex<UiState>>,
    commands: mpsc::Sender<Command>,
    cancel: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

type SaveTask = tokio::task::JoinHandle<Result<Config>>;
type PickerTask = tokio::task::JoinHandle<Option<(DirectoryTarget, PathBuf)>>;

impl Worker {
    pub fn spawn(
        config: Config,
        has_saved_config: bool,
        config_path: PathBuf,
        ctx: egui::Context,
    ) -> Result<Self> {
        let initial_snapshots: Vec<_> = Protocol::ALL
            .into_iter()
            .map(|protocol| ServiceSnapshot {
                protocol,
                state: ServiceState::Stopped,
                endpoints: Vec::new(),
                error: None,
            })
            .collect();
        let state = Arc::new(Mutex::new(UiState {
            snapshots: Some(initial_snapshots.clone()),
            active_config: Some(config.clone()),
            operation: None,
            saving: false,
            picking: None,
            notice: None,
            saved: None,
            picked: None,
            logs: VecDeque::new(),
            service_errors: Vec::new(),
            settings_in_use: has_saved_config,
            stopped: false,
            pxe_task: None,
            pxe_check: None,
            pxe_download: None,
        }));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .context(UiNotice::new(
                "Could not create the service runtime",
                "无法创建服务运行时",
            ))?;
        let (commands, mut receiver) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let shared = Arc::clone(&state);
        let thread = std::thread::Builder::new()
            .name("filemux-core".into())
            .spawn(move || {
                runtime.block_on(async {
                    let mut engine = Engine::new(config.clone());
                    let mut events = engine.subscribe();
                    let mut saved_config = has_saved_config.then_some(config);
                    let mut published = initial_snapshots;
                    // Configure failures occur before Engine can attach an error to a snapshot.
                    let mut operation_errors = Vec::new();
                    let mut save: Option<SaveTask> = None;
                    let mut picker: Option<PickerTask> = None;
                    let mut pxe: Option<tokio::task::JoinHandle<PxeResult>> = None;
                    let mut health = tokio::time::interval(Duration::from_millis(500));
                    loop {
                        tokio::select! {
                            biased;
                            _ = stop.cancelled() => break,
                            result = async { save.as_mut().expect("pending save").await }, if save.is_some() => {
                                save = None;
                                finish_save(result, &mut saved_config, engine.config(), &shared);
                                publish_snapshots(&engine, &saved_config, &operation_errors, &mut published, &shared);
                                ctx.request_repaint();
                            }
                            result = async { picker.as_mut().expect("pending picker").await }, if picker.is_some() => {
                                picker = None;
                                let mut ui = shared.lock().expect("UI state mutex");
                                ui.picking = None;
                                match result {
                                    Ok(Some(folder)) => ui.picked = Some(folder),
                                    Ok(None) => {}
                                    Err(error) => ui.notice = Some(UiNotice::with_detail(
                                        "The directory picker task failed",
                                        "选择目录任务异常",
                                        error.to_string(),
                                    )),
                                }
                                drop(ui);
                                ctx.request_repaint();
                            }
                            result = async { pxe.as_mut().expect("pending PXE task").await }, if pxe.is_some() => {
                                pxe = None;
                                let mut ui = shared.lock().expect("UI state mutex");
                                let task = ui.pxe_task.take();
                                match result {
                                    Ok(PxeResult::Check(result)) => ui.pxe_check = Some(result),
                                    Ok(PxeResult::Download(result)) => ui.pxe_download = Some(result),
                                    Err(error) => {
                                        let message = format!("{error:#}");
                                        if task == Some(PxeTask::Check) { ui.pxe_check = Some(Err(message)); }
                                        else { ui.pxe_download = Some(Err(message)); }
                                    }
                                }
                                drop(ui);
                                ctx.request_repaint();
                            }
                            command = receiver.recv() => {
                                let Some(command) = command else { break };
                                match command {
                                    Command::CheckPxe(config) => {
                                        pxe = Some(tokio::spawn(async move {
                                            let result = tokio::task::spawn_blocking(move || filemux_core::pxe::check_resources(&config))
                                                .await.map_err(|error| format!("{error:#}"))
                                                .and_then(|result| result.map_err(|error| format!("{error:#}")));
                                            PxeResult::Check(result)
                                        }));
                                    }
                                    Command::DownloadBootTool(tool) => {
                                        let config = saved_config.clone();
                                        let cancel = stop.child_token();
                                        pxe = Some(tokio::spawn(async move {
                                            let result = match config {
                                                Some(config) => filemux_core::pxe::download::download_tool(&config, tool, cancel)
                                                    .await.map_err(|error| format!("{error:#}")),
                                                None => Err("Save settings before downloading / 请先保存设置再下载".into()),
                                            };
                                            PxeResult::Download(result)
                                        }));
                                    }
                                    Command::Save(config) => {
                                        let path = config_path.clone();
                                        save = Some(tokio::task::spawn_blocking(move || {
                                            config.save(path)?;
                                            Ok(config)
                                        }));
                                    }
                                    Command::PickDirectory(target, initial, language) => {
                                        picker = Some(tokio::spawn(async move {
                                            let title = match target {
                                                DirectoryTarget::Root => language.text("Select shared directory", "选择共享目录"),
                                                DirectoryTarget::State => language.text("Select persistent state directory", "选择持久状态目录"),
                                            };
                                            let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
                                            if initial.is_dir() {
                                                dialog = dialog.set_directory(initial);
                                            }
                                            dialog.pick_folder().await.map(|folder| (target, folder.path().to_path_buf()))
                                        }));
                                    }
                                    command => {
                                        let operation = command.operation().expect("service command");
                                        operation_errors.retain(|(protocol, _)| !operation.affects(*protocol));
                                        let result = match operation {
                                            ServiceOperation::Start(protocol) => {
                                                match saved_config.as_ref() {
                                                    Some(config) => match engine.configure(config.clone()) {
                                                        Ok(()) => {
                                                            shared.lock().expect("UI state mutex").active_config = Some(engine.config().clone());
                                                            engine.start(protocol).await
                                                        }
                                                        Err(error) => Err(error),
                                                    },
                                                    None => Err(anyhow::Error::new(UiNotice::new(
                                                        "Save settings before starting a service",
                                                        "请先保存设置，再启动服务",
                                                    ))),
                                                }
                                            }
                                            ServiceOperation::Stop(protocol) => engine.stop(protocol).await,
                                            ServiceOperation::StopAll => engine.stop_all().await,
                                        };
                                        let mut summaries = Vec::new();
                                        if let Err(error) = &result {
                                            let snapshots = engine.snapshot();
                                            // start() checks every listener first; do not attribute another
                                            // protocol's newly discovered health failure to the requested one.
                                            let foreign_failure = snapshots.iter().any(|snapshot| {
                                                !operation.affects(snapshot.protocol)
                                                    && snapshot.error.is_some()
                                                    && published.iter().any(|previous| {
                                                        previous.protocol == snapshot.protocol
                                                            && previous.error != snapshot.error
                                                    })
                                            });
                                            for protocol in Protocol::ALL {
                                                if !operation.affects(protocol) {
                                                    continue;
                                                }
                                                let snapshot_error = snapshots.iter().find(|snapshot| snapshot.protocol == protocol)
                                                    .and_then(|snapshot| snapshot.error.as_deref());
                                                if matches!(operation, ServiceOperation::StopAll) {
                                                    if let Some(message) = snapshot_error {
                                                        summaries.push((protocol, snapshot_error_notice(message)));
                                                    }
                                                } else {
                                                    let full = format!("{error:#}");
                                                    if foreign_failure && snapshot_error != Some(full.as_str()) {
                                                        continue;
                                                    }
                                                    if snapshot_error != Some(full.as_str()) {
                                                        operation_errors.push((protocol, full.clone()));
                                                        push_log(&shared, Event {
                                                            at: SystemTime::now(), protocol, message: full, is_error: true,
                                                        });
                                                    }
                                                    summaries.push((protocol, service_error_summary(error, protocol)));
                                                }
                                            }
                                        }
                                        publish_snapshots(&engine, &saved_config, &operation_errors, &mut published, &shared);
                                        let mut ui = shared.lock().expect("UI state mutex");
                                        ui.operation = None;
                                        for (protocol, summary) in summaries {
                                            set_service_error(&mut ui, protocol, Some(summary));
                                        }
                                        drop(ui);
                                        ctx.request_repaint();
                                    }
                                }
                            }
                            _ = health.tick() => {
                                // Engine records the complete per-service failures and emits their log events.
                                if engine.check_health().await.is_err()
                                    && publish_snapshots(&engine, &saved_config, &operation_errors, &mut published, &shared) {
                                    ctx.request_repaint();
                                }
                            }
                            event = events.recv() => {
                                if let Ok(event) = event {
                                    push_log(&shared, event);
                                    ctx.request_repaint();
                                }
                            }
                        }
                    }
                    receiver.close();
                    if let Some(task) = picker.take() {
                        // Native dialogs may own an OS thread; never wait for the user to dismiss one.
                        task.abort();
                    }
                    let result = engine.stop_all().await;
                    if let Some(task) = pxe.take() {
                        // stop is already cancelled; await rooted temporary-file cleanup.
                        let _ = task.await;
                    }
                    while let Ok(event) = events.try_recv() {
                        push_log(&shared, event);
                    }
                    // A started atomic save must finish, but only after the actual listeners have stopped.
                    if let Some(task) = save.take() {
                        finish_save(task.await, &mut saved_config, engine.config(), &shared);
                    }
                    operation_errors.clear();
                    publish_snapshots(&engine, &saved_config, &operation_errors, &mut published, &shared);
                    let mut ui = shared.lock().expect("UI state mutex");
                    ui.operation = None;
                    ui.saving = false;
                    ui.picking = None;
                    ui.pxe_task = None;
                    ui.stopped = true;
                    if let Err(error) = result {
                        eprintln!("Failed to stop GUI services: {error:#}");
                    }
                    drop(ui);
                    ctx.request_repaint();
                });
                // In particular, a platform picker must not keep GUI shutdown waiting on a blocking thread.
                runtime.shutdown_timeout(Duration::ZERO);
            })
            .context(UiNotice::new(
                "Could not create the background service thread",
                "无法创建后台服务线程",
            ))?;
        Ok(Self {
            state,
            commands,
            cancel,
            thread: Some(thread),
        })
    }

    pub fn request(&self, command: Command) -> Result<(), UiNotice> {
        let mut ui = self.state.lock().expect("UI state mutex");
        if self.cancel.is_cancelled() || ui.stopped {
            return Err(UiNotice::new(
                "The application is closing and cannot accept new operations",
                "程序正在关闭，无法接受新操作",
            ));
        }
        if ui.pxe_task.is_some() && !matches!(&command, Command::Stop(_) | Command::StopAll) {
            return Err(UiNotice::new(
                "Wait for the PXE file task; services can still be stopped",
                "请等待 PXE 文件任务完成；仍可停止服务",
            ));
        }
        match &command {
            Command::CheckPxe(_) | Command::DownloadBootTool(_) => {
                if ui.saving || ui.picking.is_some() || ui.operation.is_some() {
                    return Err(UiNotice::new(
                        "Wait for the current operation before checking or downloading PXE files",
                        "请等待当前操作完成，再检查或下载 PXE 文件",
                    ));
                }
                ui.pxe_task = Some(match &command {
                    Command::CheckPxe(_) => {
                        ui.pxe_check = None;
                        PxeTask::Check
                    }
                    Command::DownloadBootTool(tool) => {
                        ui.pxe_download = None;
                        PxeTask::Download(*tool)
                    }
                    _ => unreachable!(),
                });
            }
            Command::Save(_) => {
                if ui.saving {
                    return Err(UiNotice::new(
                        "Settings are being saved; wait for the save to finish",
                        "设置正在保存，请等待保存完成",
                    ));
                }
                if ui.picking.is_some() {
                    return Err(UiNotice::new(
                        "Complete or cancel directory selection before saving settings",
                        "请先完成或取消目录选择，再保存设置",
                    ));
                }
                if ui.operation.is_some() {
                    return Err(UiNotice::new(
                        "Wait for the current service operation before saving settings",
                        "请等待当前服务操作完成，再保存设置",
                    ));
                }
                ui.saving = true;
            }
            Command::PickDirectory(target, _, _) => {
                if ui.picking.is_some() {
                    return Err(UiNotice::new(
                        "A directory picker is already open; complete or cancel it first",
                        "目录选择窗口已打开，请先完成或取消选择",
                    ));
                }
                if ui.saving {
                    return Err(UiNotice::new(
                        "Wait for settings to finish saving before selecting a directory",
                        "请等待设置保存完成，再选择目录",
                    ));
                }
                ui.picking = Some(*target);
            }
            _ => {
                if matches!(&command, Command::Start(_)) && ui.saving {
                    return Err(UiNotice::new(
                        "Wait for settings to finish saving before starting a service",
                        "请等待设置保存完成，再启动服务",
                    ));
                }
                if ui.operation.is_some() {
                    return Err(UiNotice::new(
                        "A service operation is still in progress; please wait",
                        "服务启停操作尚未完成，请稍候",
                    ));
                }
                ui.operation = command.operation();
            }
        }
        if let Err(error) = self.commands.try_send(command) {
            let reason = match &error {
                mpsc::error::TrySendError::Full(_) => UiNotice::new(
                    "The background operation queue is full; try again later",
                    "后台操作队列已满，请稍后重试",
                ),
                mpsc::error::TrySendError::Closed(_) => UiNotice::new(
                    "The background service thread has exited and cannot accept operations",
                    "后台服务线程已退出，无法接受操作",
                ),
            };
            match error.into_inner() {
                Command::Save(_) => ui.saving = false,
                Command::PickDirectory(_, _, _) => ui.picking = None,
                Command::CheckPxe(_) | Command::DownloadBootTool(_) => ui.pxe_task = None,
                _ => ui.operation = None,
            }
            return Err(reason);
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("The background service thread exited unexpectedly");
            }
        }
    }
}

impl ServiceOperation {
    fn affects(self, protocol: Protocol) -> bool {
        match self {
            Self::Start(target) | Self::Stop(target) => target == protocol,
            Self::StopAll => true,
        }
    }
}

fn finish_save(
    result: std::result::Result<Result<Config>, tokio::task::JoinError>,
    saved_config: &mut Option<Config>,
    active_config: &Config,
    shared: &Mutex<UiState>,
) {
    let mut ui = shared.lock().expect("UI state mutex");
    ui.saving = false;
    ui.notice = match result {
        Ok(Ok(config)) => {
            *saved_config = Some(config.clone());
            ui.settings_in_use = config == *active_config;
            ui.saved = Some(config);
            None
        }
        Ok(Err(error)) => Some(UiNotice::with_detail(
            "Could not save settings",
            "无法保存设置",
            format!("{error:#}"),
        )),
        Err(error) => Some(UiNotice::with_detail(
            "The settings save task failed",
            "保存设置任务异常",
            error.to_string(),
        )),
    };
}

fn push_log(shared: &Mutex<UiState>, event: Event) {
    let mut ui = shared.lock().expect("UI state mutex");
    if ui.logs.len() == MAX_LOGS {
        ui.logs.pop_front();
    }
    ui.logs.push_back(event);
}

fn set_service_error(ui: &mut UiState, protocol: Protocol, summary: Option<UiNotice>) {
    ui.service_errors.retain(|(target, _)| *target != protocol);
    if let Some(summary) = summary {
        ui.service_errors.push((protocol, summary));
    }
}

fn publish_snapshots(
    engine: &Engine,
    saved_config: &Option<Config>,
    operation_errors: &[(Protocol, String)],
    published: &mut Vec<ServiceSnapshot>,
    shared: &Mutex<UiState>,
) -> bool {
    let mut snapshots = engine.snapshot();
    for (protocol, error) in operation_errors {
        if let Some(snapshot) = snapshots
            .iter_mut()
            .find(|snapshot| snapshot.protocol == *protocol)
        {
            snapshot.error = Some(error.clone());
            if snapshot.state == ServiceState::Stopped {
                snapshot.state = ServiceState::Failed;
            }
        }
    }
    let mut ui = shared.lock().expect("UI state mutex");
    let settings_in_use = saved_config
        .as_ref()
        .is_some_and(|config| config == engine.config());
    let mut changed = ui.settings_in_use != settings_in_use;
    ui.settings_in_use = settings_in_use;
    let snapshots_changed = snapshots
        .iter()
        .zip(published.iter())
        .any(|(current, previous)| {
            current.state != previous.state
                || current.error != previous.error
                || current.endpoints.len() != previous.endpoints.len()
                || current
                    .endpoints
                    .iter()
                    .zip(&previous.endpoints)
                    .any(|(a, b)| {
                        a.role != b.role || a.transport != b.transport || a.address != b.address
                    })
        });
    if snapshots_changed {
        for (current, previous) in snapshots.iter().zip(published.iter()) {
            if current.error != previous.error {
                set_service_error(
                    &mut ui,
                    current.protocol,
                    current.error.as_deref().map(snapshot_error_notice),
                );
            }
        }
        ui.snapshots = Some(snapshots.clone());
        *published = snapshots;
        changed = true;
    }
    changed
}

fn service_error_summary(error: &anyhow::Error, protocol: Protocol) -> UiNotice {
    if let Some(notice) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<UiNotice>())
    {
        return notice.clone();
    }
    let reason = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<io::Error>())
        .map(|error| match error.kind() {
            io::ErrorKind::AddrInUse if protocol == Protocol::ProxyDhcp => UiText::new(
                "The ProxyDHCP address is in use. Use external DHCP mode or another server.",
                "ProxyDHCP 监听地址已被占用，请改用外部 DHCP 模式或另一台服务器。",
            ),
            io::ErrorKind::AddrInUse => UiText::new(
                "The listening address is in use. Change the port or stop the program using it.",
                "监听地址已被占用，请修改端口，或停止占用该地址的程序。",
            ),
            io::ErrorKind::AddrNotAvailable => UiText::new(
                "The listening IP is unavailable. Choose an address assigned to this computer.",
                "监听 IP 不可用，请选择本机已有的 IP 地址。",
            ),
            io::ErrorKind::PermissionDenied => UiText::new(
                "Permission denied. Check directory access and listening-port permissions.",
                "权限不足，请检查目录访问权限与监听端口权限。",
            ),
            io::ErrorKind::NotFound => UiText::new(
                "A file or directory is missing. Check the shared directory and boot-file paths.",
                "所需文件或目录不存在，请检查共享目录与启动文件路径。",
            ),
            io::ErrorKind::NotADirectory => {
                UiText::new("The path is not a directory", "路径不是目录")
            }
            io::ErrorKind::AlreadyExists => {
                UiText::new("The destination already exists", "目标已存在")
            }
            io::ErrorKind::TimedOut => UiText::new("The operation timed out", "操作超时"),
            _ => UiText::new("The service operation failed", "服务操作失败"),
        })
        .unwrap_or(UiText::new("The service operation failed", "服务操作失败"));
    let detail = error
        .chain()
        .find_map(|cause| diagnostic_address(&cause.to_string()))
        .map(|address| address.to_string())
        .unwrap_or_else(|| {
            short_diagnostic(
                &error
                    .chain()
                    .last()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| error.to_string()),
            )
        });
    UiNotice::with_detail(reason.en, reason.zh, detail)
}

fn snapshot_error_notice(message: &str) -> UiNotice {
    let detail = diagnostic_address(message)
        .map(|address| address.to_string())
        .unwrap_or_else(|| short_diagnostic(message));
    UiNotice::with_detail("The service operation failed", "服务操作失败", detail)
}

fn diagnostic_address(message: &str) -> Option<SocketAddr> {
    message
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '，' | '；' | '：')
        })
        .find_map(|token| {
            token
                .trim_matches(['(', ')', ',', ';'])
                .trim_end_matches(':')
                .parse()
                .ok()
        })
}

fn short_diagnostic(message: &str) -> String {
    let mut characters = message.lines().next().unwrap_or(message).trim().chars();
    let mut text: String = characters.by_ref().take(96).collect();
    if characters.next().is_some() {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Instant;

    fn wait_for(worker: &Worker, predicate: impl Fn(&UiState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if predicate(&worker.state.lock().expect("UI state mutex")) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not reach the expected state"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn http_address(worker: &Worker) -> SocketAddr {
        worker
            .state
            .lock()
            .unwrap()
            .snapshots
            .as_ref()
            .unwrap()
            .iter()
            .find(|snapshot| snapshot.protocol == Protocol::Http)
            .unwrap()
            .endpoints[0]
            .address
    }

    fn get_file(address: SocketAddr) -> Vec<u8> {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /shared.txt HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let body = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        assert!(response.starts_with(b"HTTP/1.1 200 "), "{response:?}");
        response[body..].to_vec()
    }

    #[test]
    fn saving_a_new_root_does_not_reconfigure_running_http() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let state_a = tempfile::tempdir().unwrap();
        let state_b = tempfile::tempdir().unwrap();
        let settings = tempfile::tempdir().unwrap();
        std::fs::write(root_a.path().join("shared.txt"), b"root A").unwrap();
        std::fs::write(root_b.path().join("shared.txt"), b"root B").unwrap();
        let mut config_a = Config {
            root: root_a.path().to_path_buf(),
            state_dir: state_a.path().to_path_buf(),
            ..Config::default()
        };
        config_a.http.bind = "127.0.0.1:0".parse().unwrap();
        let config_b = Config {
            root: root_b.path().to_path_buf(),
            state_dir: state_b.path().to_path_buf(),
            ..config_a.clone()
        };
        let config_path = settings.path().join("filemux.toml");
        config_a.save(&config_path).unwrap();
        let worker = Worker::spawn(
            config_a.clone(),
            true,
            config_path.clone(),
            egui::Context::default(),
        )
        .unwrap();
        worker.request(Command::Start(Protocol::Http)).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        let address_a = http_address(&worker);
        assert_eq!(get_file(address_a), b"root A");

        worker.request(Command::Save(config_b.clone())).unwrap();
        wait_for(&worker, |ui| !ui.saving);
        assert_eq!(Config::load(&config_path).unwrap(), config_b);
        {
            let ui = worker.state.lock().unwrap();
            assert_eq!(ui.active_config.as_ref(), Some(&config_a));
            assert!(!ui.settings_in_use);
        }
        assert_eq!(get_file(address_a), b"root A");

        worker.request(Command::Stop(Protocol::Http)).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        worker.request(Command::Start(Protocol::Http)).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        assert_eq!(
            worker.state.lock().unwrap().active_config.as_ref(),
            Some(&config_b)
        );
        assert_eq!(get_file(http_address(&worker)), b"root B");
        worker.shutdown();
        wait_for(&worker, |ui| ui.stopped);
    }

    #[test]
    fn rpc_conflict_is_reported_for_nfs_only_and_retry_preserves_running_http() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let settings = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("shared.txt"), b"HTTP stays available").unwrap();
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = Config {
            root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            ..Config::default()
        };
        config.http.bind = "127.0.0.1:0".parse().unwrap();
        config.nfs.bind = "127.0.0.1:0".parse().unwrap();
        config.nfs.rpcbind = Some(blocker.local_addr().unwrap());
        let path = settings.path().join("filemux.toml");
        config.save(&path).unwrap();
        let worker = Worker::spawn(config, true, path, egui::Context::default()).unwrap();
        worker.request(Command::Start(Protocol::Http)).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        let http = http_address(&worker);

        worker.request(Command::Start(Protocol::Nfs)).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        {
            let ui = worker.state.lock().unwrap();
            let snapshots = ui.snapshots.as_ref().unwrap();
            let nfs = snapshots
                .iter()
                .find(|snapshot| snapshot.protocol == Protocol::Nfs)
                .unwrap();
            assert_eq!(nfs.state, ServiceState::Failed);
            assert!(nfs.error.is_some());
            assert_eq!(
                ui.service_errors
                    .iter()
                    .map(|(protocol, _)| *protocol)
                    .collect::<Vec<_>>(),
                [Protocol::Nfs]
            );
            let notice = &ui.service_errors[0].1;
            assert!(
                notice
                    .detail
                    .as_deref()
                    .expect("the original bind failure must remain available")
                    .contains(&blocker.local_addr().unwrap().to_string())
            );
            assert!(
                snapshots
                    .iter()
                    .any(|snapshot| snapshot.protocol == Protocol::Http
                        && snapshot.state == ServiceState::Running
                        && snapshot.error.is_none())
            );
        }
        assert_eq!(get_file(http), b"HTTP stays available");

        drop(blocker);
        worker.request(Command::Start(Protocol::Nfs)).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        {
            let ui = worker.state.lock().unwrap();
            let nfs = ui
                .snapshots
                .as_ref()
                .unwrap()
                .iter()
                .find(|snapshot| snapshot.protocol == Protocol::Nfs)
                .unwrap();
            assert_eq!(nfs.state, ServiceState::Running);
            assert!(nfs.error.is_none());
            assert!(
                !ui.service_errors
                    .iter()
                    .any(|(protocol, _)| *protocol == Protocol::Nfs)
            );
        }
        assert_eq!(get_file(http), b"HTTP stays available");
        worker.shutdown();
        wait_for(&worker, |ui| ui.stopped);
        assert!(TcpStream::connect_timeout(&http, Duration::from_millis(200)).is_err());
    }

    #[test]
    fn tftp_conflict_preserves_the_exact_address_for_recovery() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let settings = tempfile::tempdir().unwrap();
        let blocker = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = blocker.local_addr().unwrap();
        let mut config = Config {
            root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            ..Config::default()
        };
        config.tftp.bind = address;
        let path = settings.path().join("filemux.toml");
        config.save(&path).unwrap();
        let worker = Worker::spawn(config, true, path, egui::Context::default()).unwrap();
        worker.request(Command::Start(Protocol::Tftp)).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        {
            let ui = worker.state.lock().unwrap();
            assert_eq!(
                ui.service_errors
                    .iter()
                    .find(|(protocol, _)| *protocol == Protocol::Tftp)
                    .and_then(|(_, notice)| notice.detail.as_deref())
                    .and_then(|detail| detail.parse::<SocketAddr>().ok()),
                Some(address),
            );
        }
        worker.shutdown();
        wait_for(&worker, |ui| ui.stopped);
    }

    #[test]
    fn stopping_services_does_not_hide_a_failed_save() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let settings = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("shared.txt"), b"save failure").unwrap();
        let mut config = Config {
            root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            ..Config::default()
        };
        config.http.bind = "127.0.0.1:0".parse().unwrap();
        let path = settings.path().join("filemux.toml");
        config.save(&path).unwrap();
        let worker =
            Worker::spawn(config.clone(), true, path.clone(), egui::Context::default()).unwrap();
        worker.request(Command::Start(Protocol::Http)).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        assert_eq!(get_file(http_address(&worker)), b"save failure");
        let invalid = Config {
            root: root.path().join("missing"),
            ..config.clone()
        };
        worker.request(Command::Save(invalid)).unwrap();
        wait_for(&worker, |ui| !ui.saving);
        let save_error = worker
            .state
            .lock()
            .unwrap()
            .notice
            .clone()
            .expect("failed save must remain visible");
        assert!(
            save_error
                .detail
                .as_deref()
                .expect("the original save failure must remain available")
                .contains("missing")
        );
        worker.request(Command::StopAll).unwrap();
        wait_for(&worker, |ui| ui.operation.is_none());
        let current_error = worker.state.lock().unwrap().notice.clone();
        assert_eq!(current_error.as_ref(), Some(&save_error));
        assert_eq!(Config::load(&path).unwrap(), config);
        worker.shutdown();
        wait_for(&worker, |ui| ui.stopped);
    }
}
