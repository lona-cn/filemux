#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod connections;
mod i18n;
mod settings;
mod worker;

use anyhow::Result;
use chrono::{DateTime, Local};
use clap::Parser;
use connections::{AccessScope, ConnectionInfo};
use eframe::egui::{self, Color32, FontFamily, FontId, RichText, Stroke, TextStyle};
use filemux_core::engine::ServiceSnapshot;
use filemux_core::pxe::{PxeCheckReport, download::DownloadReport};
use filemux_core::{Config, Event, Protocol, ServiceState, Transport, default_config_path};
use i18n::{Language, LanguagePreference, PREF_LANGUAGE_KEY, UiNotice, UiText};
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::time::Duration;
use worker::{Command, DirectoryTarget, MAX_LOGS, PxeTask, ServiceOperation, Worker};

const BACKGROUND: Color32 = Color32::from_rgb(29, 33, 39);
const PANEL: Color32 = Color32::from_rgb(34, 38, 45);
const BORDER: Color32 = Color32::from_rgb(65, 71, 81);
const ACCENT: Color32 = Color32::from_rgb(104, 172, 255);
const PRIMARY: Color32 = Color32::from_rgb(39, 105, 197);
const MUTED: Color32 = Color32::from_rgb(157, 165, 178);
const GREEN: Color32 = Color32::from_rgb(91, 211, 122);
const WARNING: Color32 = Color32::from_rgb(242, 190, 87);
const RED: Color32 = Color32::from_rgb(247, 118, 118);

#[derive(Parser)]
#[command(
    name = "filemux-gui",
    version,
    about = "Filemux desktop file-sharing server"
)]
struct Args {
    #[arg(short, long, help = "Configuration file shared with the CLI")]
    config: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    let startup_language = Language::detect();
    let path = args.config.unwrap_or_else(default_config_path);
    let (config, saved, warning) = if path.exists() {
        match Config::load(&path) {
            Ok(config) => (config, true, None),
            Err(error) => (
                Config::default(),
                false,
                Some(UiNotice::with_detail(
                    "Could not read the configuration. Default settings are shown; saving will replace this file.",
                    "配置未能读取，当前显示默认设置；保存会替换该配置文件。",
                    format!("{error:#}"),
                )),
            ),
        }
    } else {
        (Config::default(), false, None)
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 680.0])
            .with_min_inner_size([900.0, 600.0]),
        persist_window: false,
        persistence_path: Some(preferences_path(&path)),
        ..Default::default()
    };
    if let Err(error) = eframe::run_native(
        "Filemux",
        options,
        Box::new(move |cc| Ok(Box::new(FilemuxApp::new(cc, config, saved, path, warning)?))),
    ) {
        eprintln!(
            "{}: {error}",
            startup_language.text("Could not open Filemux", "无法打开 Filemux 窗口")
        );
        std::process::exit(1);
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Form {
    config: Config,
    root: String,
    state_dir: String,
    http_ip: String,
    tftp_ip: String,
    nfs_ip: String,
    rpcbind_enabled: bool,
    rpcbind: String,
    pxe_server_ip: String,
}

impl Form {
    fn new(config: Config) -> Self {
        Self {
            root: config.root.to_string_lossy().into_owned(),
            state_dir: config.state_dir.to_string_lossy().into_owned(),
            http_ip: config.http.bind.ip().to_string(),
            tftp_ip: config.tftp.bind.ip().to_string(),
            nfs_ip: config.nfs.bind.ip().to_string(),
            rpcbind_enabled: config.nfs.rpcbind.is_some(),
            pxe_server_ip: config.pxe.server_ip.to_string(),
            rpcbind: config
                .nfs
                .rpcbind
                .map(|address| address.to_string())
                .unwrap_or_else(|| "127.0.0.1:111".into()),
            config,
        }
    }

    fn build(&self) -> std::result::Result<Config, UiNotice> {
        let mut config = self.config.clone();
        config.root = absolute_path(&self.root)?;
        config.state_dir = absolute_path(&self.state_dir)?;
        config
            .http
            .bind
            .set_ip(self.http_ip.trim().parse::<IpAddr>().map_err(|error| {
                UiNotice::with_detail(
                    "Enter a valid HTTP listening IP address.",
                    "请输入有效的 HTTP 监听 IP 地址。",
                    error.to_string(),
                )
            })?);
        config
            .tftp
            .bind
            .set_ip(self.tftp_ip.trim().parse::<IpAddr>().map_err(|error| {
                UiNotice::with_detail(
                    "Enter a valid TFTP listening IP address.",
                    "请输入有效的 TFTP 监听 IP 地址。",
                    error.to_string(),
                )
            })?);
        config
            .nfs
            .bind
            .set_ip(self.nfs_ip.trim().parse::<IpAddr>().map_err(|error| {
                UiNotice::with_detail(
                    "Enter a valid NFS listening IP address.",
                    "请输入有效的 NFS 监听 IP 地址。",
                    error.to_string(),
                )
            })?);
        config.nfs.rpcbind = if self.rpcbind_enabled {
            Some(self.rpcbind.trim().parse::<std::net::SocketAddr>().map_err(|error| {
                UiNotice::with_detail(
                    "Enter the RPC discovery address as IP:port, for example 127.0.0.1:111.",
                    "RPC 端口发现地址应为 IP:端口，例如 127.0.0.1:111。",
                    error.to_string(),
                )
            })?)
        } else {
            None
        };
        config.pxe.server_ip = if self.pxe_server_ip.trim().is_empty() && !config.pxe.enabled {
            std::net::Ipv4Addr::UNSPECIFIED
        } else {
            self.pxe_server_ip
                .trim()
                .parse()
                .map_err(|error: std::net::AddrParseError| {
                    UiNotice::with_detail(
                        "Enter a valid PXE server IPv4 address.",
                        "请输入有效的 PXE 服务 IPv4 地址。",
                        error.to_string(),
                    )
                })?
        };
        Ok(config)
    }
}

fn absolute_path(value: &str) -> std::result::Result<PathBuf, UiNotice> {
    if value.trim().is_empty() {
        return Err(UiNotice::new("Choose a directory.", "请选择目录。"));
    }
    let path = PathBuf::from(value);
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| {
                UiNotice::with_detail(
                    "Could not resolve this relative directory.",
                    "无法解析此相对目录。",
                    error.to_string(),
                )
            })?
            .join(path)
    })
}

fn preferences_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(".filemux-gui.ron")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Services,
    Settings,
    Logs,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SettingsTab {
    General,
    Http,
    Tftp,
    Nfs,
    Pxe,
}

impl From<Protocol> for SettingsTab {
    fn from(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Http => Self::Http,
            Protocol::Tftp => Self::Tftp,
            Protocol::Nfs => Self::Nfs,
            Protocol::ProxyDhcp => Self::Pxe,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ConfigStatus {
    NotSaved,
    Unsaved,
    SavedIdle,
    SavedActive,
    SavedNotInUse,
}

fn config_status(saved: bool, dirty: bool, running: bool, in_use: bool) -> ConfigStatus {
    if dirty {
        ConfigStatus::Unsaved
    } else if !saved {
        ConfigStatus::NotSaved
    } else if !running {
        ConfigStatus::SavedIdle
    } else if in_use {
        ConfigStatus::SavedActive
    } else {
        ConfigStatus::SavedNotInUse
    }
}

struct LogLine {
    time: String,
    protocol: Protocol,
    message: String,
    is_error: bool,
}

impl From<Event> for LogLine {
    fn from(event: Event) -> Self {
        let time: DateTime<Local> = event.at.into();
        Self {
            time: time.format("%H:%M:%S").to_string(),
            protocol: event.protocol,
            message: event.message,
            is_error: event.is_error,
        }
    }
}

fn matches_log(line: &LogLine, protocol: Option<Protocol>, errors_only: bool) -> bool {
    protocol.is_none_or(|protocol| protocol == line.protocol) && (!errors_only || line.is_error)
}

struct ListenLine {
    address: String,
    detail: String,
    transport: &'static str,
    rpc_discovery: bool,
}

struct ServiceView {
    snapshot: ServiceSnapshot,
    listeners: Vec<ListenLine>,
    summary: Option<UiNotice>,
    error_address: Option<SocketAddr>,
    connection: Option<ConnectionInfo>,
}

impl ServiceView {
    fn new(snapshot: ServiceSnapshot, language: Language) -> Self {
        let mut listeners = Vec::new();
        for (index, endpoint) in snapshot.endpoints.iter().enumerate() {
            if snapshot.endpoints[..index]
                .iter()
                .any(|earlier| earlier.role == endpoint.role && earlier.address == endpoint.address)
            {
                continue;
            }
            let tcp = snapshot.endpoints.iter().any(|other| {
                other.role == endpoint.role
                    && other.address == endpoint.address
                    && other.transport == Transport::Tcp
            });
            let udp = snapshot.endpoints.iter().any(|other| {
                other.role == endpoint.role
                    && other.address == endpoint.address
                    && other.transport == Transport::Udp
            });
            let transport = match (tcp, udp) {
                (true, true) => "TCP / UDP",
                (true, false) => "TCP",
                _ => "UDP",
            };
            let detail = match endpoint.role {
                "nfs" => format!("NFS / MOUNT · {transport}"),
                "portmapper" => format!(
                    "{} · {transport}",
                    language.text("RPC discovery", "RPC 端口发现")
                ),
                _ => transport.to_owned(),
            };
            listeners.push(ListenLine {
                address: endpoint.address.to_string(),
                detail,
                transport,
                rpc_discovery: endpoint.role == "portmapper",
            });
        }
        Self {
            snapshot,
            listeners,
            summary: None,
            error_address: None,
            connection: None,
        }
    }
}

struct FilemuxApp {
    page: Page,
    settings_tab: SettingsTab,
    system_language: Language,
    language_preference: LanguagePreference,
    form: Form,
    initial_form: Form,
    saved_form: Option<Form>,
    active_config: Config,
    config_path: PathBuf,
    read_warning: Option<UiNotice>,
    worker: Worker,
    services: Vec<ServiceView>,
    operation: Option<ServiceOperation>,
    saving: bool,
    picking: Option<DirectoryTarget>,
    settings_in_use: bool,
    notice: Option<UiNotice>,
    logs: VecDeque<LogLine>,
    log_protocol: Option<Protocol>,
    errors_only: bool,
    save_requested: bool,
    discard_requested: bool,
    confirm_close: bool,
    close_after_save: bool,
    closing: bool,
    allow_close: bool,
    pxe_task: Option<PxeTask>,
    pxe_check: Option<std::result::Result<PxeCheckReport, String>>,
    pxe_check_form: Option<Form>,
    pxe_download: Option<std::result::Result<DownloadReport, String>>,
    pxe_menu_url: Option<(SocketAddrV4, String)>,
    listener_focus: Option<SocketAddr>,
}

impl FilemuxApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        saved: bool,
        config_path: PathBuf,
        warning: Option<UiNotice>,
    ) -> Result<Self> {
        install_style(&cc.egui_ctx);
        let system_language = Language::detect();
        let saved_language = cc
            .storage
            .and_then(|storage| storage.get_string(PREF_LANGUAGE_KEY));
        let language_preference = LanguagePreference::from_saved(saved_language.as_deref());
        let language = language_preference.resolve(system_language);
        let form = Form::new(config.clone());
        let worker = Worker::spawn(
            config.clone(),
            saved,
            config_path.clone(),
            cc.egui_ctx.clone(),
        )?;
        let snapshots = worker
            .state
            .lock()
            .expect("UI state mutex")
            .snapshots
            .take()
            .expect("initial service snapshots");
        Ok(Self {
            page: Page::Services,
            settings_tab: SettingsTab::General,
            system_language,
            language_preference,
            initial_form: form.clone(),
            saved_form: saved.then(|| form.clone()),
            form,
            active_config: config,
            config_path,
            read_warning: warning,
            worker,
            services: snapshots
                .into_iter()
                .map(|snapshot| ServiceView::new(snapshot, language))
                .collect(),
            operation: None,
            saving: false,
            picking: None,
            settings_in_use: saved,
            notice: None,
            logs: VecDeque::new(),
            log_protocol: None,
            errors_only: false,
            save_requested: false,
            discard_requested: false,
            confirm_close: false,
            close_after_save: false,
            closing: false,
            allow_close: false,
            pxe_task: None,
            pxe_check: None,
            pxe_check_form: None,
            pxe_download: None,
            pxe_menu_url: None,
            listener_focus: None,
        })
    }

    fn language(&self) -> Language {
        self.language_preference.resolve(self.system_language)
    }

    fn set_language_preference(&mut self, preference: LanguagePreference) {
        let previous = self.language();
        self.language_preference = preference;
        if preference == LanguagePreference::System {
            self.system_language = Language::detect();
        }
        let language = self.language();
        if previous != language {
            for view in &mut self.services {
                for listener in &mut view.listeners {
                    if listener.rpc_discovery {
                        listener.detail = format!(
                            "{} · {}",
                            language.text("RPC discovery", "RPC 端口发现"),
                            listener.transport
                        );
                    }
                }
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let language = self.language();
        let mut state = self.worker.state.lock().expect("UI state mutex");
        let snapshots_changed = state.snapshots.is_some();
        if let Some(snapshots) = state.snapshots.take() {
            self.services = snapshots
                .into_iter()
                .map(|snapshot| {
                    let mut view = ServiceView::new(snapshot, language);
                    if view.snapshot.error.is_some() {
                        if let Some(previous) = self.services.iter_mut().find(|previous| {
                            previous.snapshot.protocol == view.snapshot.protocol
                                && previous.snapshot.error == view.snapshot.error
                        }) {
                            view.summary = previous.summary.take();
                            view.error_address = previous.error_address;
                        }
                    }
                    view
                })
                .collect();
        }
        if let Some(config) = state.active_config.take() {
            self.active_config = config;
        }
        self.operation = state.operation;
        self.saving = state.saving;
        self.picking = state.picking;
        self.pxe_task = state.pxe_task;
        if let Some(result) = state.pxe_check.take() {
            self.pxe_check = Some(result);
        }
        if let Some(result) = state.pxe_download.take() {
            self.pxe_download = Some(result);
        }
        self.settings_in_use = state.settings_in_use;
        if let Some(notice) = state.notice.take() {
            self.notice = Some(notice);
        }
        let mut saved_now = false;
        if let Some(config) = state.saved.take() {
            self.form = Form::new(config);
            self.saved_form = Some(self.form.clone());
            self.read_warning = None;
            self.notice = None;
            saved_now = true;
        }
        if let Some((target, path)) = state.picked.take() {
            let value = path.to_string_lossy().into_owned();
            match target {
                DirectoryTarget::Root => self.form.root = value,
                DirectoryTarget::State => self.form.state_dir = value,
            }
        }
        for (protocol, summary) in state.service_errors.drain(..) {
            if let Some(view) = self
                .services
                .iter_mut()
                .find(|view| view.snapshot.protocol == protocol)
            {
                view.error_address = summary
                    .detail
                    .as_deref()
                    .and_then(|detail| detail.parse().ok());
                view.summary = Some(summary);
            }
        }
        for event in state.logs.drain(..) {
            if self.logs.len() == MAX_LOGS {
                self.logs.pop_front();
            }
            self.logs.push_back(LogLine::from(event));
        }
        let stopped = state.stopped;
        drop(state);
        if snapshots_changed {
            let addresses = if self.services.iter().any(|view| {
                matches!(view.snapshot.protocol, Protocol::Http | Protocol::Nfs)
                    && view.snapshot.state == ServiceState::Running
                    && view
                        .snapshot
                        .endpoints
                        .iter()
                        .any(|endpoint| endpoint.address.ip().is_unspecified())
            }) {
                connections::local_addresses()
            } else {
                Ok(Vec::new())
            };
            let addresses = addresses.as_deref().map_err(|_| ());
            for view in &mut self.services {
                view.connection =
                    ConnectionInfo::new(&view.snapshot, &self.active_config, addresses);
            }
        }
        if self.close_after_save && self.saving && self.running() && self.operation.is_none() {
            self.request(Command::StopAll);
        }
        if self.close_after_save && !self.saving {
            self.close_after_save = false;
            if saved_now {
                self.begin_shutdown();
            } else {
                self.page = Page::Settings;
            }
        }
        if stopped && self.closing {
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn request(&mut self, command: Command) {
        let operation = match &command {
            Command::Start(protocol) => Some(ServiceOperation::Start(*protocol)),
            Command::Stop(protocol) => Some(ServiceOperation::Stop(*protocol)),
            Command::StopAll => Some(ServiceOperation::StopAll),
            _ => None,
        };
        let saving = matches!(&command, Command::Save(_));
        let picking = if let Command::PickDirectory(target, _, _) = &command {
            Some(*target)
        } else {
            None
        };
        let pxe_task = match &command {
            Command::CheckPxe(_) => Some(PxeTask::Check),
            Command::DownloadBootTool(tool) => Some(PxeTask::Download(*tool)),
            _ => None,
        };
        match self.worker.request(command) {
            Ok(()) => {
                if operation.is_some() {
                    self.operation = operation;
                }
                if saving {
                    self.saving = true;
                }
                if picking.is_some() {
                    self.picking = picking;
                }
                if let Some(task) = pxe_task {
                    self.pxe_task = Some(task);
                    if task == PxeTask::Check {
                        self.pxe_check = None;
                        self.pxe_check_form = Some(self.form.clone());
                    } else {
                        self.pxe_download = None;
                    }
                }
                if saving || picking.is_some() {
                    self.notice = None;
                }
            }
            Err(error) => {
                self.notice = Some(error);
            }
        }
    }

    fn dirty(&self) -> bool {
        self.saved_form.as_ref().unwrap_or(&self.initial_form) != &self.form
    }

    fn running(&self) -> bool {
        self.services.iter().any(|view| {
            matches!(
                view.snapshot.state,
                ServiceState::Running | ServiceState::Starting | ServiceState::Stopping
            )
        })
    }

    fn config_status_label(&self) -> &'static str {
        let language = self.language();
        match config_status(
            self.saved_form.is_some(),
            self.dirty(),
            self.running(),
            self.settings_in_use,
        ) {
            ConfigStatus::NotSaved => language.text("No saved configuration", "尚未保存配置"),
            ConfigStatus::Unsaved => language.text("Unsaved changes", "修改尚未保存"),
            ConfigStatus::SavedIdle => language.text(
                "Settings saved; no services running",
                "设置已保存，当前没有运行中的服务",
            ),
            ConfigStatus::SavedActive => {
                language.text("Using saved settings", "当前使用已保存设置")
            }
            ConfigStatus::SavedNotInUse => language.text(
                "Saved; services still use previous settings",
                "设置已保存，运行中的服务仍使用原设置",
            ),
        }
    }
    fn config_status_color(&self) -> Color32 {
        if self.read_warning.is_some() {
            RED
        } else if self.dirty() || (self.running() && !self.settings_in_use) {
            WARNING
        } else if self.saved_form.is_some() {
            GREEN
        } else {
            MUTED
        }
    }

    fn edits_allowed(&self) -> bool {
        !self.saving && !self.closing && self.pxe_task.is_none()
    }

    fn save_blocker(&self) -> Option<UiText> {
        if self.closing {
            Some(UiText::new(
                "Stopping services and closing the window",
                "正在停止服务并关闭窗口",
            ))
        } else if self.saving {
            Some(UiText::new("Saving settings", "正在保存设置"))
        } else if self.pxe_task.is_some() {
            Some(UiText::new(
                "Wait for the PXE file task.",
                "请等待 PXE 文件任务完成。",
            ))
        } else if self.picking.is_some() {
            Some(UiText::new(
                "Complete or cancel directory selection before saving.",
                "请先完成或取消目录选择，再保存设置。",
            ))
        } else if self.operation.is_some() {
            Some(UiText::new(
                "Wait for the current service operation before saving.",
                "请等待当前服务操作完成，再保存设置。",
            ))
        } else if self.saved_form.is_some() && !self.dirty() {
            Some(UiText::new("No changes to save", "没有需要保存的修改"))
        } else {
            None
        }
    }

    fn start_blocker(&self) -> Option<UiText> {
        if self.closing {
            Some(UiText::new(
                "Stopping services and closing the window",
                "正在停止服务并关闭窗口",
            ))
        } else if self.operation.is_some() {
            Some(UiText::new(
                "Wait for the current service operation.",
                "请等待当前服务操作完成。",
            ))
        } else if self.saving {
            Some(UiText::new(
                "Wait for settings to finish saving before starting a service.",
                "正在保存设置，请稍候再启动服务。",
            ))
        } else if self.pxe_task.is_some() {
            Some(UiText::new(
                "Wait for the PXE file task before starting a service.",
                "请等待 PXE 文件任务完成，再启动服务。",
            ))
        } else if self.saved_form.is_none() {
            Some(UiText::new(
                "Confirm the shared directory in Settings and save first.",
                "请先到设置页确认共享目录并保存配置。",
            ))
        } else if self.dirty() {
            Some(UiText::new(
                "Save or discard your changes before starting a service.",
                "修改尚未保存，请先保存或放弃修改。",
            ))
        } else if self.running() && !self.settings_in_use {
            Some(UiText::new(
                "Services still use previous settings. Stop all services before restarting.",
                "运行中的服务仍使用原设置，请全部停止后再启动。",
            ))
        } else {
            None
        }
    }

    // Footer actions are applied after this frame's editors consume their input.
    fn save_settings(&mut self) {
        self.save_requested = true;
    }
    fn discard_changes(&mut self) {
        self.discard_requested = true;
    }

    fn apply_editor_actions(&mut self) {
        if std::mem::take(&mut self.discard_requested) {
            self.form
                .clone_from(self.saved_form.as_ref().unwrap_or(&self.initial_form));
            self.notice = None;
        }
        if std::mem::take(&mut self.save_requested) {
            if let Some(reason) = self.save_blocker() {
                self.notice = Some(UiNotice::new(reason.en, reason.zh));
                self.close_after_save = false;
            } else {
                match self.form.build() {
                    Ok(config) => {
                        self.request(Command::Save(config));
                        if !self.saving {
                            self.close_after_save = false;
                        }
                    }
                    Err(error) => {
                        self.notice = Some(error);
                        self.close_after_save = false;
                        self.page = Page::Settings;
                    }
                }
            }
        }
    }

    fn open_settings(&mut self, tab: SettingsTab) {
        self.settings_tab = tab;
        self.page = Page::Settings;
        self.listener_focus = None;
    }

    fn show_protocol_logs(&mut self, protocol: Protocol) {
        self.log_protocol = Some(protocol);
        self.errors_only = false;
        self.page = Page::Logs;
    }

    fn draw_header(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Filemux").size(24.0).strong());
            ui.add_space(22.0);
            for (page, title) in [
                (Page::Services, lang.text("Services", "服务")),
                (Page::Settings, lang.text("Settings", "设置")),
                (Page::Logs, lang.text("Logs", "日志")),
            ] {
                let selected = self.page == page;
                let response = ui.add_enabled(
                    !self.closing,
                    egui::Button::new(RichText::new(title).size(17.0).color(if selected {
                        Color32::WHITE
                    } else {
                        MUTED
                    }))
                    .frame(false)
                    .min_size(egui::vec2(68.0, 38.0)),
                );
                if selected {
                    let rect = response.rect.shrink2(egui::vec2(6.0, 0.0));
                    ui.painter().line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(2.0_f32, ACCENT),
                    );
                }
                if response.clicked() {
                    self.page = page;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.page == Page::Services {
                    let enabled = self.running() && self.operation.is_none() && !self.closing;
                    let reason = if self.closing {
                        lang.text("Stopping services", "正在停止服务")
                    } else if self.operation.is_some() {
                        lang.text(
                            "Wait for the current service operation.",
                            "请等待当前服务操作完成。",
                        )
                    } else {
                        lang.text("No services are running", "当前没有运行中的服务")
                    };
                    if ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(lang.text("Stop all", "全部停止"))
                                .min_size(egui::vec2(110.0, 34.0)),
                        )
                        .on_disabled_hover_text(reason)
                        .clicked()
                    {
                        self.request(Command::StopAll);
                    }
                }
                if self.closing {
                    ui.label(
                        RichText::new(lang.text("Stopping services", "正在停止内部服务"))
                            .small()
                            .color(MUTED),
                    );
                    ui.spinner();
                } else if let Some(operation) = self.operation {
                    ui.label(
                        RichText::new(operation_label(operation, lang))
                            .small()
                            .color(MUTED),
                    );
                    ui.spinner();
                } else if self.close_after_save {
                    ui.label(
                        RichText::new(lang.text("Saving, then closing", "正在保存，完成后关闭"))
                            .small()
                            .color(MUTED),
                    );
                    ui.spinner();
                }
            });
        });
    }

    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        let lang = self.language();
        ui.horizontal(|ui| {
            let show_status = self.page != Page::Settings;
            let right_width = if show_status {
                330.0_f32.min(ui.available_width() * 0.48)
            } else {
                0.0
            };
            let left_width = (ui.available_width() - right_width - 12.0).max(100.0);
            ui.allocate_ui_with_layout(
                egui::vec2(left_width, 18.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        RichText::new(lang.text("Config:", "配置文件："))
                            .small()
                            .color(MUTED),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(self.config_path.to_string_lossy())
                                .small()
                                .color(MUTED),
                        )
                        .truncate(),
                    )
                    .on_hover_text(self.config_path.to_string_lossy());
                },
            );
            if show_status {
                ui.allocate_ui_with_layout(
                    egui::vec2(right_width, 18.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(self.config_status_label())
                                    .small()
                                    .color(self.config_status_color()),
                            )
                            .truncate(),
                        )
                        .on_hover_text(self.config_status_label());
                    },
                );
            }
        });
    }

    fn draw_services(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        let mut edit_root = false;
        egui::Frame::group(ui.style())
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if self.running() {
                            lang.text("Sharing", "正在共享")
                        } else {
                            lang.text("Shared folder", "共享目录")
                        })
                        .strong(),
                    );
                    let available = (ui.available_width() - 126.0).max(100.0);
                    let root = if self.running() {
                        self.active_config.root.to_string_lossy()
                    } else if let Some(saved) = &self.saved_form {
                        std::borrow::Cow::Borrowed(saved.root.as_str())
                    } else {
                        std::borrow::Cow::Borrowed(
                            lang.text("No saved configuration", "尚未保存配置"),
                        )
                    };
                    ui.add_sized(
                        [available, 28.0],
                        egui::Label::new(RichText::new(root.as_ref()).monospace()).truncate(),
                    )
                    .on_hover_text(root.as_ref());
                    edit_root = ui
                        .add_enabled(
                            !self.closing,
                            egui::Button::new(lang.text("Change", "修改目录"))
                                .min_size(egui::vec2(100.0, 30.0)),
                        )
                        .clicked();
                });
            });
        if edit_root {
            self.open_settings(SettingsTab::General);
        }
        ui.add_space(12.0);
        if let Some(reason) = self.start_blocker() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(self.config_status_color(), reason.get(lang));
                if self.saved_form.is_none()
                    && ui
                        .button(lang.text("Set up sharing", "设置共享目录"))
                        .clicked()
                {
                    self.open_settings(SettingsTab::General);
                }
            });
            if self.read_warning.is_some() {
                ui.colored_label(
                    RED,
                    lang.text("Could not read the configuration. Check Settings; no services were started with defaults.", "配置文件未能读取，请到设置页检查。当前没有使用默认设置启动服务。"),
                );
            }
            ui.add_space(8.0);
        }
        egui::Frame::group(ui.style())
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                let widths = service_widths(ui.available_width());
                service_columns(ui, widths, 24.0, |column, ui| {
                    ui.colored_label(
                        MUTED,
                        [
                            lang.text("Service", "服务"),
                            lang.text("Status", "状态"),
                            lang.text("Listening on", "实际监听"),
                            lang.text("Access", "访问权限"),
                            lang.text("Actions", "操作"),
                        ][column],
                    );
                });
                ui.separator();
                for index in 0..self.services.len() {
                    let mut action = None;
                    let view = &self.services[index];
                    let protocol = view.snapshot.protocol;
                    let state = match self.operation {
                        Some(ServiceOperation::Start(target)) if target == protocol => {
                            ServiceState::Starting
                        }
                        Some(ServiceOperation::Stop(target)) if target == protocol => {
                            ServiceState::Stopping
                        }
                        Some(ServiceOperation::StopAll)
                            if view.snapshot.state == ServiceState::Running =>
                        {
                            ServiceState::Stopping
                        }
                        _ => view.snapshot.state,
                    };
                    let start_reason = self.start_blocker().or_else(|| {
                        if protocol != Protocol::ProxyDhcp { return None; }
                        let config = self.saved_form.as_ref().map(|form| &form.config)?;
                        if !config.pxe.enabled || config.pxe.mode != filemux_core::PxeMode::ProxyDhcp {
                            Some(UiText::new("Enable ProxyDHCP mode in PXE settings.", "请到 PXE 设置启用 ProxyDHCP 模式。"))
                        } else if ![Protocol::Http, Protocol::Tftp].iter().all(|dependency|
                            self.services.iter().any(|service| service.snapshot.protocol == *dependency && service.snapshot.state == ServiceState::Running)) {
                            Some(UiText::new("Start HTTP and TFTP before ProxyDHCP.", "请先启动 HTTP 和 TFTP，再启动 ProxyDHCP。"))
                        } else { None }
                    });
                    let stop_reason = if matches!(protocol, Protocol::Http | Protocol::Tftp)
                        && self.services.iter().any(|service| service.snapshot.protocol == Protocol::ProxyDhcp && service.snapshot.state == ServiceState::Running) {
                        Some(UiText::new("Stop ProxyDHCP first.", "请先停止 ProxyDHCP。"))
                    } else { None };
                    let can_stop = self.operation.is_none() && !self.closing && stop_reason.is_none();
                    let access_config = if view.snapshot.state == ServiceState::Running {
                        Some(&self.active_config)
                    } else {
                        self.saved_form.as_ref().map(|saved| &saved.config)
                    };
                    let upload = access_config.is_some_and(|config| match protocol {
                        Protocol::Http => config.http.allow_upload,
                        Protocol::Tftp => config.tftp.allow_upload,
                        Protocol::Nfs | Protocol::ProxyDhcp => false,
                    });
                    let height = if view.listeners.len() > 1 { 94.0 } else { 62.0 };
                    service_columns(ui, widths, height, |column, ui| match column {
                        0 => {
                            ui.label(RichText::new(protocol_label(protocol)).size(18.0).strong());
                        }
                        1 => {
                            let color = match state {
                                ServiceState::Running => GREEN,
                                ServiceState::Failed => RED,
                                _ => MUTED,
                            };
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                                ui.painter().circle_filled(rect.center(), 4.0, color);
                                ui.colored_label(
                                    color,
                                    service_state_label(state, lang),
                                );
                            });
                        }
                        2 => {
                            if view.listeners.is_empty() {
                                ui.colored_label(MUTED, lang.text("Not listening", "未监听"));
                            }
                            for listener in &view.listeners {
                                ui.add(
                                    egui::Label::new(RichText::new(&listener.address).monospace())
                                        .wrap()
                                        .selectable(true),
                                );
                                ui.label(RichText::new(&listener.detail).small().color(MUTED));
                            }
                            if state == ServiceState::Running {
                                if let Some(connection) = &view.connection {
                                    ui.label(
                                        RichText::new(connection.scope.label(lang))
                                            .small()
                                            .color(if connection.scope == AccessScope::LocalOnly {
                                                WARNING
                                            } else {
                                                MUTED
                                            }),
                                    );
                                }
                            }
                        }
                        3 => {
                            ui.label(if protocol == Protocol::ProxyDhcp {
                                lang.text("Boot information only", "仅提供启动信息")
                            } else if protocol == Protocol::Nfs {
                                lang.text("Read-only", "只读")
                            } else if upload {
                                lang.text("Upload enabled", "允许上传")
                            } else {
                                lang.text("Download only", "仅下载")
                            });
                            if state == ServiceState::Stopped {
                                ui.label(
                                    RichText::new(if self.saved_form.is_some() {
                                        lang.text("Saved", "已保存设置")
                                    } else {
                                        lang.text("Not running", "尚未运行")
                                    })
                                    .small()
                                    .color(MUTED),
                                );
                            }
                        }
                        _ => {
                            ui.horizontal(|ui| {
                                let running = state == ServiceState::Running;
                                let enabled = if running {
                                    can_stop
                                } else {
                                    start_reason.is_none()
                                };
                                let label = match state {
                                    ServiceState::Running => lang.text("Stop", "停止"),
                                    ServiceState::Failed => lang.text("Retry", "重试"),
                                    ServiceState::Starting => lang.text("Starting", "启动中"),
                                    ServiceState::Stopping => lang.text("Stopping", "停止中"),
                                    _ => lang.text("Start", "启动"),
                                };
                                if ui
                                    .add_enabled(
                                        enabled,
                                        egui::Button::new(label).min_size(egui::vec2(70.0, 32.0)),
                                    )
                                    .on_disabled_hover_text(
                                        (if running { stop_reason } else { start_reason }).map(|reason| reason.get(lang)).unwrap_or(lang.text("Wait for the current service operation.", "请等待当前服务操作完成。")),
                                    )
                                    .clicked()
                                {
                                    action = Some(if running {
                                        ServiceAction::Stop(protocol)
                                    } else {
                                        ServiceAction::Start(protocol)
                                    });
                                }
                                if ui
                                    .add_enabled(
                                        !self.closing,
                                        egui::Button::new(lang.text("Settings", "修改设置"))
                                            .min_size(egui::vec2(96.0, 32.0)),
                                    )
                                    .clicked()
                                {
                                    action = Some(ServiceAction::Settings(protocol));
                                }
                            });
                        }
                    });
                    if state == ServiceState::Running {
                        if let Some(connection) = &view.connection {
                            let change_address = ui.push_id(("client-connection", protocol), |ui| {
                                draw_connection(ui, protocol, connection, lang)
                            }).inner;
                            if change_address {
                                action = Some(ServiceAction::Settings(protocol));
                            }
                        }
                    }
                    if view.snapshot.error.is_some() {
                        egui::Frame::new()
                            .fill(Color32::from_rgb(59, 35, 40))
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            view.summary.as_ref().map(|notice| notice.text.get(lang))
                                                .unwrap_or(lang.text("The service failed. View technical details or logs.", "服务发生错误，请查看技术详情或日志。")),
                                        ).color(RED),
                                    ).wrap(),
                                );
                                ui.horizontal_wrapped(|ui| {
                                    if let Some(address) = view.error_address {
                                        ui.monospace(address.to_string());
                                    }
                                    if ui.button(lang.text("Change settings", "修改对应设置")).clicked() {
                                        action = Some(match view.error_address {
                                            Some(address) if protocol != Protocol::ProxyDhcp => {
                                                ServiceAction::ListenerSettings(protocol, address)
                                            }
                                            _ => ServiceAction::Settings(protocol),
                                        });
                                    }
                                    if ui.button(lang.text("View logs", "查看日志")).clicked() {
                                        action = Some(ServiceAction::Logs(protocol));
                                    }
                                });
                                egui::CollapsingHeader::new(lang.text("Technical details", "技术详情"))
                                    .id_salt(("service-error", protocol))
                                    .show(ui, |ui| {
                                        if let Some(error) = &view.snapshot.error {
                                            ui.add(egui::Label::new(RichText::new(error).monospace()).wrap().selectable(true));
                                        }
                                    });
                            });
                    }
                    if index + 1 < self.services.len() {
                        ui.separator();
                    }
                    match action {
                        Some(ServiceAction::Start(protocol)) => {
                            self.request(Command::Start(protocol))
                        }
                        Some(ServiceAction::Stop(protocol)) => {
                            self.request(Command::Stop(protocol))
                        }
                        Some(ServiceAction::Settings(protocol)) => {
                            self.open_settings(protocol.into())
                        }
                        Some(ServiceAction::ListenerSettings(protocol, address)) => {
                            self.open_settings(protocol.into());
                            self.listener_focus = Some(address);
                        }
                        Some(ServiceAction::Logs(protocol)) => self.show_protocol_logs(protocol),
                        None => {}
                    }
                }
            });
    }

    fn draw_logs(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        ui.horizontal_wrapped(|ui| {
            ui.label(lang.text("Protocol", "协议"));
            ui.selectable_value(&mut self.log_protocol, None, lang.text("All", "全部"));
            for protocol in Protocol::ALL {
                ui.selectable_value(
                    &mut self.log_protocol,
                    Some(protocol),
                    protocol_label(protocol),
                );
            }
            ui.add_space(12.0);
            ui.checkbox(&mut self.errors_only, lang.text("Errors only", "仅错误"));
            ui.label(
                RichText::new(lang.text(
                    "Raw service output · Up to 500 entries",
                    "原始服务记录 · 最多保留 500 条",
                ))
                .small()
                .color(MUTED),
            );
        });
        ui.separator();
        if let Some(protocol) = self.log_protocol {
            if let Some(error) = self
                .services
                .iter()
                .find(|view| view.snapshot.protocol == protocol)
                .and_then(|view| view.snapshot.error.as_deref())
                .filter(|error| {
                    !self.logs.iter().any(|line| {
                        line.protocol == protocol && line.is_error && line.message == *error
                    })
                })
            {
                ui.colored_label(
                    RED,
                    lang.text("Last service error (raw)", "最近一次服务错误（原文）"),
                );
                egui::ScrollArea::vertical()
                    .id_salt("service-error-detail")
                    .max_height(110.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(error).monospace().color(RED))
                                .wrap()
                                .selectable(true),
                        );
                    });
                ui.separator();
            }
        }
        egui::ScrollArea::vertical()
            .id_salt("protocol-logs")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let mut visible = false;
                for line in self
                    .logs
                    .iter()
                    .filter(|line| matches_log(line, self.log_protocol, self.errors_only))
                {
                    visible = true;
                    let color = if line.is_error {
                        RED
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.horizontal_top(|ui| {
                        ui.add_sized(
                            [68.0, 20.0],
                            egui::Label::new(RichText::new(&line.time).monospace().color(MUTED)),
                        );
                        ui.add_sized(
                            [60.0, 20.0],
                            egui::Label::new(
                                RichText::new(protocol_label(line.protocol)).color(MUTED),
                            ),
                        );
                        ui.add_sized(
                            [38.0, 20.0],
                            egui::Label::new(
                                RichText::new(if line.is_error {
                                    lang.text("Error", "错误")
                                } else {
                                    lang.text("Info", "信息")
                                })
                                .color(color),
                            ),
                        );
                        ui.add(
                            egui::Label::new(RichText::new(&line.message).monospace().color(color))
                                .wrap()
                                .selectable(true),
                        );
                    });
                    ui.add_space(6.0);
                }
                if !visible {
                    ui.colored_label(
                        MUTED,
                        if self.logs.is_empty() {
                            lang.text(
                                "No service events yet. Start a service to see its activity.",
                                "尚无服务记录。启动服务后会显示实际操作结果。",
                            )
                        } else {
                            lang.text("No events match these filters.", "当前筛选条件下没有日志。")
                        },
                    );
                }
            });
    }

    fn begin_shutdown(&mut self) {
        self.confirm_close = false;
        self.closing = true;
        self.worker.shutdown();
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        let lang = self.language();
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.closing {
                if self.saving {
                    self.close_after_save = true;
                } else if self.dirty() {
                    self.confirm_close = true;
                } else {
                    self.begin_shutdown();
                }
            }
        }
        if self.confirm_close {
            egui::Modal::new(egui::Id::new("confirm-close")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.heading(lang.text("Unsaved changes", "还有未保存的修改"));
                ui.label(lang.text(
                    "Closing this window stops its services. Save your changes or discard them.",
                    "关闭窗口会停止本窗口的服务。请选择保存设置，或明确放弃修改。",
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(lang.text("Keep editing", "返回编辑")).clicked() {
                        self.confirm_close = false;
                        self.page = Page::Settings;
                    }
                    if ui
                        .button(lang.text("Discard & close", "放弃修改并关闭"))
                        .clicked()
                    {
                        self.begin_shutdown();
                    }
                    if ui
                        .add_enabled(
                            self.save_blocker().is_none(),
                            egui::Button::new(
                                RichText::new(lang.text("Save & close", "保存并关闭"))
                                    .color(Color32::WHITE),
                            )
                            .fill(PRIMARY),
                        )
                        .clicked()
                    {
                        self.confirm_close = false;
                        self.close_after_save = true;
                        self.save_settings();
                    }
                });
                if let Some(reason) = self.save_blocker() {
                    ui.colored_label(MUTED, reason.get(lang));
                }
            });
        }
    }
}

enum ServiceAction {
    Start(Protocol),
    Stop(Protocol),
    Settings(Protocol),
    ListenerSettings(Protocol, SocketAddr),
    Logs(Protocol),
}

fn protocol_label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Http => "HTTP",
        Protocol::Tftp => "TFTP",
        Protocol::Nfs => "NFSv3",
        Protocol::ProxyDhcp => "ProxyDHCP",
    }
}

fn operation_label(operation: ServiceOperation, language: Language) -> &'static str {
    match operation {
        ServiceOperation::Start(Protocol::Http) => language.text("Starting HTTP", "正在启动 HTTP"),
        ServiceOperation::Start(Protocol::Tftp) => language.text("Starting TFTP", "正在启动 TFTP"),
        ServiceOperation::Start(Protocol::Nfs) => language.text("Starting NFSv3", "正在启动 NFSv3"),
        ServiceOperation::Start(Protocol::ProxyDhcp) => {
            language.text("Starting ProxyDHCP", "正在启动 ProxyDHCP")
        }
        ServiceOperation::Stop(Protocol::Http) => language.text("Stopping HTTP", "正在停止 HTTP"),
        ServiceOperation::Stop(Protocol::Tftp) => language.text("Stopping TFTP", "正在停止 TFTP"),
        ServiceOperation::Stop(Protocol::Nfs) => language.text("Stopping NFSv3", "正在停止 NFSv3"),
        ServiceOperation::Stop(Protocol::ProxyDhcp) => {
            language.text("Stopping ProxyDHCP", "正在停止 ProxyDHCP")
        }
        ServiceOperation::StopAll => language.text("Stopping all services", "正在停止所有服务"),
    }
}

fn service_state_label(state: ServiceState, language: Language) -> &'static str {
    match state {
        ServiceState::Stopped => language.text("Stopped", "已停止"),
        ServiceState::Starting => language.text("Starting", "启动中"),
        ServiceState::Running => language.text("Running", "运行中"),
        ServiceState::Stopping => language.text("Stopping", "停止中"),
        ServiceState::Failed => language.text("Failed", "失败"),
    }
}

fn draw_notice(ui: &mut egui::Ui, notice: &UiNotice, language: Language) {
    ui.add(egui::Label::new(RichText::new(notice.text.get(language)).color(RED)).wrap());
    if let Some(detail) = &notice.detail {
        ui.add(
            egui::Label::new(RichText::new(detail).small().monospace().color(RED))
                .wrap()
                .selectable(true),
        );
    }
}

fn copy_button(ui: &mut egui::Ui, language: Language, text: &str, label: &str) {
    let id = ui.make_persistent_id(("copy-feedback", label));
    let time = ui.input(|input| input.time);
    let copied = ui
        .ctx()
        .data(|data| data.get_temp::<f64>(id))
        .is_some_and(|until| until > time);
    if ui
        .add(
            egui::Button::new(if copied {
                language.text("Copied", "已复制")
            } else {
                label
            })
            .min_size(egui::vec2(104.0, 28.0)),
        )
        .clicked()
    {
        ui.ctx().copy_text(text.to_owned());
        ui.ctx().data_mut(|data| data.insert_temp(id, time + 2.0));
        ui.ctx().request_repaint_after(Duration::from_secs(2));
    }
}

fn draw_connection(
    ui: &mut egui::Ui,
    protocol: Protocol,
    connection: &ConnectionInfo,
    language: Language,
) -> bool {
    if !matches!(protocol, Protocol::Http | Protocol::Nfs) {
        return false;
    }
    let mut change_address = false;
    if connection.targets.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(WARNING, if connection.lookup_failed {
                language.text(
                    "Could not read local interfaces. Set a specific local listening IP.",
                    "无法读取本机网络接口，请在协议设置中指定明确的本机监听 IP。",
                )
            } else {
                language.text(
                    "No client address can be generated for this listener. Set a specific local IP.",
                    "无法为此监听地址生成客户端地址，请在协议设置中指定可用的本机 IP。",
                )
            });
            change_address = ui.button(language.text("Change listening IP", "修改监听 IP")).clicked();
        });
        return change_address;
    }
    if protocol == Protocol::Http {
        for (index, url) in connection.targets.iter().enumerate() {
            ui.push_id(index, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(url).monospace())
                            .wrap()
                            .selectable(true),
                    );
                    copy_button(ui, language, url, language.text("Copy address", "复制地址"));
                    if ui
                        .button(language.text("Open in browser", "在浏览器打开"))
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url.clone()));
                    }
                });
            });
        }
    } else {
        egui::CollapsingHeader::new(language.text("Linux mount examples", "Linux 挂载示例"))
            .id_salt("nfs-client-examples")
            .show(ui, |ui| {
                ui.label(language.text(
                    "On the client, create /mnt/filemux first, then run a command below.",
                    "先在客户端创建 /mnt/filemux 目录，再执行下方命令。",
                ));
                for (index, command) in connection.targets.iter().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(command).monospace())
                                .wrap()
                                .selectable(true),
                        );
                        copy_button(
                            ui,
                            language,
                            command,
                            language.text("Copy command", "复制命令"),
                        );
                    });
                }
            });
    }
    if connection.scope == AccessScope::LocalOnly {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(language.text(
                    "Other devices cannot use this loopback address.",
                    "其他设备无法通过此本机地址访问。",
                ))
                .small()
                .color(WARNING),
            );
            change_address = ui
                .small_button(language.text("Set up LAN access", "配置局域网访问"))
                .clicked();
        });
    } else {
        ui.label(RichText::new(language.text(
            "Use an address reachable from the client. Firewall rules are not changed automatically.",
            "请选择客户端能够访问的地址；不会自动修改防火墙。",
        )).small().color(MUTED));
    }
    change_address
}

fn service_widths(width: f32) -> [f32; 5] {
    [
        112.0,
        120.0,
        (width - 112.0 - 120.0 - 118.0 - 186.0 - 40.0).max(170.0),
        118.0,
        186.0,
    ]
}

fn service_columns(
    ui: &mut egui::Ui,
    widths: [f32; 5],
    height: f32,
    mut body: impl FnMut(usize, &mut egui::Ui),
) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for (column, width) in widths.into_iter().enumerate() {
            ui.allocate_ui_with_layout(
                egui::vec2(width, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(width);
                    body(column, ui);
                },
            );
        }
    });
}

impl eframe::App for FilemuxApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(
            PREF_LANGUAGE_KEY,
            self.language_preference.as_str().to_owned(),
        );
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        let language = self.language();
        egui::TopBottomPanel::top("navigation")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(20, 8)),
            )
            .show(ctx, |ui| self.draw_header(ui));
        egui::TopBottomPanel::bottom("configuration-status")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(16, 6)),
            )
            .show(ctx, |ui| self.draw_status_bar(ui));
        if self.page == Page::Settings {
            egui::TopBottomPanel::bottom("settings-actions")
                .frame(
                    egui::Frame::new()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::symmetric(20, 10)),
                )
                .show(ctx, |ui| self.draw_settings_actions(ui));
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                if let Some(notice) = &self.notice {
                    draw_notice(ui, notice, language);
                    ui.add_space(10.0);
                }
                match self.page {
                    Page::Services => {
                        egui::ScrollArea::vertical()
                            .id_salt("services-content")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.draw_services(ui));
                    }
                    Page::Settings => self.draw_settings(ui),
                    Page::Logs => self.draw_logs(ui),
                }
            });
        self.handle_close(ctx);
        self.apply_editor_actions();
        if self
            .pxe_check_form
            .as_ref()
            .is_some_and(|checked| checked != &self.form)
        {
            self.pxe_check = None;
            self.pxe_check_form = None;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

fn install_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sc".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/NotoSansSC-Regular.otf"))
            .into(),
    );
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .expect("proportional font family")
        .insert(0, "noto-sc".into());
    fonts
        .families
        .get_mut(&FontFamily::Monospace)
        .expect("monospace font family")
        .push("noto-sc".into());
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(48, 54, 63);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.selection.bg_fill = PRIMARY;
    style.visuals.selection.stroke.color = Color32::WHITE;
    style.visuals.hyperlink_color = ACCENT;
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(15.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(19.0, FontFamily::Proportional),
    );
    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_states_distinguish_first_use_edits_and_running_settings() {
        assert_eq!(
            config_status(false, false, false, false),
            ConfigStatus::NotSaved
        );
        assert_eq!(
            config_status(false, true, false, false),
            ConfigStatus::Unsaved
        );
        assert_eq!(
            config_status(true, false, false, false),
            ConfigStatus::SavedIdle
        );
        assert_eq!(
            config_status(true, false, true, true),
            ConfigStatus::SavedActive
        );
        assert_eq!(
            config_status(true, false, true, false),
            ConfigStatus::SavedNotInUse
        );
        assert_eq!(
            config_status(true, true, true, false),
            ConfigStatus::Unsaved
        );
    }

    #[test]
    fn protocol_and_error_filters_do_not_mix_other_service_events() {
        let events = [
            LogLine {
                time: String::new(),
                protocol: Protocol::Http,
                message: String::new(),
                is_error: false,
            },
            LogLine {
                time: String::new(),
                protocol: Protocol::Nfs,
                message: String::new(),
                is_error: true,
            },
            LogLine {
                time: String::new(),
                protocol: Protocol::Tftp,
                message: String::new(),
                is_error: true,
            },
        ];
        assert_eq!(
            events
                .iter()
                .filter(|line| matches_log(line, Some(Protocol::Nfs), true))
                .map(|line| line.protocol)
                .collect::<Vec<_>>(),
            [Protocol::Nfs]
        );
        assert_eq!(
            events
                .iter()
                .filter(|line| matches_log(line, None, true))
                .map(|line| line.protocol)
                .collect::<Vec<_>>(),
            [Protocol::Nfs, Protocol::Tftp]
        );
        assert_eq!(
            events
                .iter()
                .filter(|line| matches_log(line, Some(Protocol::Http), true))
                .count(),
            0
        );
    }
}
