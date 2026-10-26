#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod worker;

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use clap::Parser;
use eframe::egui::{self, Color32, FontFamily, FontId, RichText, Stroke, TextStyle};
use filemux_core::{Config, Protocol, ServiceState, default_config_path};
use filemux_engine::ServiceSnapshot;
use std::collections::VecDeque;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;
use worker::{Command, DirectoryTarget, MAX_LOGS, Notice, Worker};

const BACKGROUND: Color32 = Color32::from_rgb(29, 33, 39);
const PANEL: Color32 = Color32::from_rgb(34, 38, 45);
const BORDER: Color32 = Color32::from_rgb(65, 71, 81);
const ACCENT: Color32 = Color32::from_rgb(48, 126, 233);
const MUTED: Color32 = Color32::from_rgb(157, 165, 178);
const GREEN: Color32 = Color32::from_rgb(91, 211, 122);
const RED: Color32 = Color32::from_rgb(247, 118, 118);

#[derive(Parser)]
#[command(name = "filemux-gui", version, about = "Filemux 桌面文件共享")]
struct Args {
    #[arg(short, long, help = "与 CLI 共用的配置文件")]
    config: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    let path = args.config.unwrap_or_else(default_config_path);
    let (config, saved, warning) = if path.exists() {
        match Config::load(&path) {
            Ok(config) => (config, true, None),
            Err(error) => (
                Config::default(),
                false,
                Some(format!(
                    "配置未能读取：{error:#}。当前显示默认设置；保存会替换该配置文件。"
                )),
            ),
        }
    } else {
        (Config::default(), false, None)
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 820.0])
            .with_min_inner_size([920.0, 700.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Filemux",
        options,
        Box::new(move |cc| Ok(Box::new(FilemuxApp::new(cc, config, saved, path, warning)?))),
    );
    if let Err(error) = result {
        eprintln!("无法打开 Filemux 窗口：{error}");
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
            rpcbind: config
                .nfs
                .rpcbind
                .map(|address| address.to_string())
                .unwrap_or_else(|| "127.0.0.1:111".into()),
            config,
        }
    }

    fn build(&self) -> Result<Config> {
        let mut config = self.config.clone();
        config.root = absolute_path(&self.root)?;
        config.state_dir = absolute_path(&self.state_dir)?;
        config.http.bind.set_ip(
            self.http_ip
                .trim()
                .parse::<IpAddr>()
                .context("HTTP 监听地址不是有效 IP 地址")?,
        );
        config.tftp.bind.set_ip(
            self.tftp_ip
                .trim()
                .parse::<IpAddr>()
                .context("TFTP 监听地址不是有效 IP 地址")?,
        );
        config.nfs.bind.set_ip(
            self.nfs_ip
                .trim()
                .parse::<IpAddr>()
                .context("NFS 监听地址不是有效 IP 地址")?,
        );
        config.nfs.rpcbind = if self.rpcbind_enabled {
            Some(
                self.rpcbind
                    .trim()
                    .parse()
                    .context("端口映射地址应为 IP:端口，例如 127.0.0.1:111")?,
            )
        } else {
            None
        };
        Ok(config)
    }
}

fn absolute_path(value: &str) -> Result<PathBuf> {
    anyhow::ensure!(!value.trim().is_empty(), "目录不能为空");
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

struct LogLine {
    text: String,
    is_error: bool,
}

struct FilemuxApp {
    form: Form,
    saved_form: Option<Form>,
    config_path: PathBuf,
    worker: Worker,
    snapshots: Vec<ServiceSnapshot>,
    busy: bool,
    settings_in_use: bool,
    notice: Option<Notice>,
    logs: VecDeque<LogLine>,
    closing: bool,
    allow_close: bool,
}

impl FilemuxApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        saved: bool,
        config_path: PathBuf,
        warning: Option<String>,
    ) -> Result<Self> {
        install_style(&cc.egui_ctx);
        let form = Form::new(config.clone());
        let worker = Worker::spawn(config, saved, config_path.clone(), cc.egui_ctx.clone())?;
        let snapshots = worker
            .state
            .lock()
            .expect("UI state mutex")
            .snapshots
            .clone();
        Ok(Self {
            saved_form: saved.then(|| form.clone()),
            form,
            config_path,
            worker,
            snapshots,
            busy: false,
            settings_in_use: false,
            notice: warning.map(|text| Notice {
                text,
                is_error: true,
            }),
            logs: VecDeque::new(),
            closing: false,
            allow_close: false,
        })
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let mut state = self.worker.state.lock().expect("UI state mutex");
        self.snapshots.clone_from(&state.snapshots);
        self.busy = state.busy;
        self.settings_in_use = state.settings_in_use;
        if let Some(notice) = state.notice.take() {
            self.notice = Some(notice);
        }
        if let Some(config) = state.saved.take() {
            let form = Form::new(config);
            self.saved_form = Some(form.clone());
            self.form = form;
        }
        if let Some((target, path)) = state.picked.take() {
            let value = path.to_string_lossy().into_owned();
            match target {
                DirectoryTarget::Root => self.form.root = value,
                DirectoryTarget::State => self.form.state_dir = value,
            }
        }
        for event in state.logs.drain(..) {
            if self.logs.len() == MAX_LOGS {
                self.logs.pop_front();
            }
            let time: DateTime<Local> = event.at.into();
            self.logs.push_back(LogLine {
                text: format!(
                    "[{}] {} {}",
                    time.format("%H:%M:%S"),
                    event.protocol,
                    event.message
                ),
                is_error: event.is_error,
            });
        }
        if state.stopped && self.closing {
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn request(&mut self, command: Command) {
        match self.worker.request(command) {
            Ok(()) => {
                self.busy = true;
            }
            Err(error) => {
                self.notice = Some(Notice {
                    text: format!("{error:#}"),
                    is_error: true,
                })
            }
        }
    }

    fn dirty(&self) -> bool {
        self.saved_form.as_ref() != Some(&self.form)
    }
    fn running(&self) -> bool {
        self.snapshots.iter().any(|s| {
            matches!(
                s.state,
                ServiceState::Running | ServiceState::Starting | ServiceState::Stopping
            )
        })
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        let mut command = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Filemux").size(38.0).strong());
            ui.add_space(10.0);
            ui.label(RichText::new("文件共享").size(27.0).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.busy || self.closing {
                    ui.spinner();
                }
                ui.label(RichText::new("HTTP / TFTP / NFSv3").small().color(MUTED));
            });
        });
        ui.add_space(16.0);
        egui::Frame::group(ui.style())
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.label("共享目录");
                ui.add_space(5.0);
                ui.add_enabled_ui(!self.busy && !self.closing, |ui| {
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 142.0).max(200.0);
                        ui.add_sized(
                            [width, 34.0],
                            egui::TextEdit::singleline(&mut self.form.root),
                        );
                        if ui
                            .add_sized([130.0, 34.0], egui::Button::new("选择目录"))
                            .clicked()
                        {
                            command = Some(Command::PickDirectory(
                                DirectoryTarget::Root,
                                PathBuf::from(&self.form.root),
                            ));
                        }
                    });
                });
            });
        ui.add_space(16.0);
        ui.label(RichText::new("服务").heading());
        ui.add_space(6.0);
        let can_start =
            !self.dirty() && self.saved_form.is_some() && (!self.running() || self.settings_in_use);
        egui::Frame::group(ui.style()).fill(PANEL).stroke(Stroke::new(1.0_f32, BORDER))
            .inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::Grid::new("services").num_columns(5).min_col_width(100.0).spacing([18.0, 12.0]).striped(true).show(ui, |ui| {
                    for title in ["协议", "监听地址", "端口", "状态", "操作"] { ui.colored_label(MUTED, title); }
                    ui.end_row();
                    for snapshot in &self.snapshots {
                        let (enabled, ip, address) = match snapshot.protocol {
                            Protocol::Http => (&mut self.form.config.http.enabled, &mut self.form.http_ip, &mut self.form.config.http.bind),
                            Protocol::Tftp => (&mut self.form.config.tftp.enabled, &mut self.form.tftp_ip, &mut self.form.config.tftp.bind),
                            Protocol::Nfs => (&mut self.form.config.nfs.enabled, &mut self.form.nfs_ip, &mut self.form.config.nfs.bind),
                        };
                        let mut port = address.port();
                        ui.add_enabled_ui(!self.busy && !self.closing, |ui| {
                            ui.vertical(|ui| {
                                let label = match snapshot.protocol { Protocol::Http => "HTTP", Protocol::Tftp => "TFTP", Protocol::Nfs => "NFSv3" };
                                ui.checkbox(enabled, label).on_hover_text("勾选后，CLI serve 会启用该协议。本窗口请使用右侧启动按钮。");
                                if snapshot.protocol == Protocol::Nfs { ui.label(RichText::new("UDP + TCP").small().color(MUTED)); }
                            });
                        });
                        ui.add_enabled_ui(!self.busy && !self.closing, |ui| {
                            ui.add_sized([215.0, 28.0], egui::TextEdit::singleline(ip));
                        });
                        ui.add_enabled_ui(!self.busy && !self.closing, |ui| {
                            ui.add_sized([85.0, 28.0], egui::DragValue::new(&mut port).range(0..=u16::MAX));
                        });
                        address.set_port(port);
                        let color = match snapshot.state { ServiceState::Running => GREEN, ServiceState::Failed => RED, _ => MUTED };
                        ui.colored_label(color, snapshot.state.label());
                        let running = snapshot.state == ServiceState::Running;
                        let enabled = !self.busy && !self.closing && (running || can_start);
                        if ui.add_enabled(enabled, egui::Button::new(if running { "停止" } else { "启动" }).min_size(egui::vec2(90.0, 30.0))).clicked() {
                            command = Some(if running { Command::Stop(snapshot.protocol) } else { Command::Start(snapshot.protocol) });
                        }
                        ui.end_row();
                    }
                });
                for snapshot in &self.snapshots {
                    if let Some(error) = &snapshot.error { ui.colored_label(RED, format!("{}：{error}", snapshot.protocol)); }
                    if !snapshot.endpoints.is_empty() {
                        let endpoints = snapshot.endpoints.iter().map(|e| format!("{} {} {}", e.role, e.transport, e.address)).collect::<Vec<_>>().join("；");
                        ui.label(RichText::new(format!("{} 实际监听：{endpoints}", snapshot.protocol)).small().color(MUTED));
                    }
                }
            });
        ui.add_space(10.0);
        ui.add_enabled_ui(!self.busy && !self.closing, |ui| {
            ui.checkbox(&mut self.form.config.tftp.allow_upload, "允许 TFTP 上传");
        });
        ui.label(
            RichText::new("默认关闭，不覆盖已有文件。HTTP 与 NFS 始终只读。")
                .small()
                .color(MUTED),
        );
        ui.add_space(4.0);
        egui::CollapsingHeader::new("持久状态与 NFS 端口映射").show(ui, |ui| {
            ui.add_enabled_ui(!self.busy && !self.closing, |ui| {
                ui.label("状态目录（必须位于共享目录之外；更换共享根时请选择另一状态目录）");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.form.state_dir).desired_width(650.0));
                    if ui.button("选择状态目录").clicked() {
                        command = Some(Command::PickDirectory(DirectoryTarget::State, PathBuf::from(&self.form.state_dir)));
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.form.rpcbind_enabled, "启用 NFS 端口映射");
                    ui.add_enabled(self.form.rpcbind_enabled, egui::TextEdit::singleline(&mut self.form.rpcbind).desired_width(240.0));
                });
                ui.horizontal(|ui| { ui.label("NFS 导出路径"); ui.text_edit_singleline(&mut self.form.config.nfs.export); });
                ui.label(RichText::new("U-Boot 通常需要 UDP 111 的端口发现。低端口可能需要系统权限；不自动修改防火墙。").small().color(MUTED));
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.busy && !self.closing,
                    egui::Button::new(RichText::new("保存设置").color(Color32::WHITE))
                        .fill(ACCENT)
                        .min_size(egui::vec2(140.0, 36.0)),
                )
                .clicked()
            {
                match self.form.build() {
                    Ok(config) => command = Some(Command::Save(config)),
                    Err(error) => {
                        self.notice = Some(Notice {
                            text: format!("{error:#}"),
                            is_error: true,
                        })
                    }
                }
            }
            if ui
                .add_enabled(
                    self.running() && !self.busy && !self.closing,
                    egui::Button::new("全部停止").min_size(egui::vec2(110.0, 36.0)),
                )
                .clicked()
            {
                command = Some(Command::StopAll);
            }
            let note = if self.dirty() {
                "当前修改尚未保存"
            } else if self.running() && !self.settings_in_use {
                "设置已保存，全部停止后再启动以使用新设置"
            } else if self.running() {
                "当前服务正在使用已保存的设置"
            } else {
                "设置已保存，尚未启动服务"
            };
            ui.label(RichText::new(note).small().color(MUTED));
        });
        ui.label(
            RichText::new(format!("配置文件：{}", self.config_path.display()))
                .small()
                .color(MUTED),
        );
        if let Some(notice) = &self.notice {
            ui.colored_label(if notice.is_error { RED } else { GREEN }, &notice.text);
        }
        if self.closing {
            ui.colored_label(MUTED, "正在停止内部服务，完成后关闭窗口……");
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("日志").heading());
            ui.label(RichText::new("最多保留 500 条").small().color(MUTED));
        });
        egui::Frame::group(ui.style())
            .fill(BACKGROUND)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .max_height(180.0)
                    .show(ui, |ui| {
                        if self.logs.is_empty() {
                            ui.colored_label(
                                MUTED,
                                "尚无服务记录。保存设置并启动服务后，这里会显示实际操作结果。",
                            );
                        }
                        for line in &self.logs {
                            ui.label(RichText::new(&line.text).monospace().size(14.0).color(
                                if line.is_error {
                                    RED
                                } else {
                                    Color32::from_rgb(210, 215, 223)
                                },
                            ));
                        }
                    });
            });
        if let Some(command) = command {
            self.request(command);
        }
    }
}

impl eframe::App for FilemuxApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.closing {
                self.closing = true;
                self.worker.shutdown();
            }
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.draw(ui));
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

fn install_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sc".into(),
        egui::FontData::from_static(include_bytes!(
            "../../../assets/fonts/NotoSansSC-Regular.otf"
        ))
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
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.hyperlink_color = ACCENT;
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(16.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(16.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(21.0, FontFamily::Proportional),
    );
    ctx.set_style(style);
}
