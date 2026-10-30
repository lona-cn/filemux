use crate::i18n::{Language, LanguagePreference};
use crate::worker::PxeTask;
use crate::worker::{Command, DirectoryTarget};
use crate::{ACCENT, BORDER, FilemuxApp, GREEN, MUTED, PANEL, RED, SettingsTab};
use eframe::egui::{self, RichText, Stroke};
use filemux_core::pxe::download::BootTool;
use filemux_core::{PxeEntry, PxeMode};
use std::net::SocketAddr;
use std::path::PathBuf;

impl FilemuxApp {
    pub(super) fn draw_settings(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        ui.push_id("settings-tabs", |ui| {
            ui.horizontal(|ui| {
                for (tab, title) in [
                    (SettingsTab::General, lang.text("General", "通用")),
                    (SettingsTab::Http, "HTTP"),
                    (SettingsTab::Tftp, "TFTP"),
                    (SettingsTab::Nfs, "NFSv3"),
                    (SettingsTab::Pxe, "PXE"),
                ] {
                    ui.selectable_value(&mut self.settings_tab, tab, title);
                    ui.add_space(10.0);
                }
            });
        });
        ui.separator();
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .id_salt("settings-content")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(warning) = &self.read_warning {
                    crate::draw_notice(ui, warning, lang);
                    ui.add_space(10.0);
                }
                if self.closing {
                    hint(
                        ui,
                        lang.text(
                            "Closing and stopping services. Editing and directory selection are unavailable.",
                            "正在关闭并停止服务，暂时不能编辑或选择目录。",
                        ),
                    );
                    ui.add_space(8.0);
                } else if self.saving {
                    hint(
                        ui,
                        lang.text(
                            "Saving settings. Editing and directory selection are unavailable; services can still be stopped on the Services page.",
                            "正在保存设置，暂时不能编辑或选择目录；服务仍可在“服务”页停止。",
                        ),
                    );
                    ui.add_space(8.0);
                }
                if self.pxe_task.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        hint(ui, lang.text("Checking or downloading PXE files. You can switch pages and stop services.", "正在检查或下载 PXE 文件；仍可切页和停止服务。"));
                    });
                }
                if let Some(target) = self.picking {
                    ui.horizontal_wrapped(|ui| {
                        ui.spinner();
                        hint(
                            ui,
                            match target {
                                DirectoryTarget::Root => lang.text(
                                    "Choosing the shared directory. Select a directory or cancel in the system dialog.",
                                    "正在选择共享目录，请在系统对话框中完成选择或取消。",
                                ),
                                DirectoryTarget::State => lang.text(
                                    "Choosing the persistent state directory. Select a directory or cancel in the system dialog.",
                                    "正在选择持久状态目录，请在系统对话框中完成选择或取消。",
                                ),
                            },
                        );
                    });
                    hint(
                        ui,
                        lang.text(
                            "Another directory cannot be selected until this dialog closes. You can still switch pages and stop services.",
                            "目录选择完成前，不能再次选择目录；仍可切换页面和停止服务。",
                        ),
                    );
                    ui.add_space(8.0);
                }

                match self.settings_tab {
                    SettingsTab::General => self.draw_general_settings(ui),
                    SettingsTab::Http => self.draw_http_settings(ui),
                    SettingsTab::Tftp => self.draw_tftp_settings(ui),
                    SettingsTab::Nfs => self.draw_nfs_settings(ui),
                    SettingsTab::Pxe => self.draw_pxe_settings(ui),
                }
                ui.add_space(12.0);
                hint(
                    ui,
                    lang.text(
                        "Saving does not restart services. Running services keep their startup settings.",
                        "保存不会重启服务。运行中的服务仍使用启动时的设置。",
                    ),
                );
                hint(
                    ui,
                    lang.text(
                        "To apply new settings, stop all running services on the Services page, then start them individually.",
                        "如有服务仍在运行，请先在“服务”页全部停止，再逐个启动以使用新设置。",
                    ),
                );
            });
    }

    pub(super) fn draw_settings_actions(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        let blocker = self.save_blocker();
        let can_discard = self.edits_allowed() && self.picking.is_none() && self.dirty();
        let status_color = if self.read_warning.is_some() {
            RED
        } else if self.saved_form.as_ref() == Some(&self.form) {
            GREEN
        } else {
            MUTED
        };
        ui.horizontal(|ui| {
            ui.colored_label(status_color, self.config_status_label());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        blocker.is_none(),
                        egui::Button::new(
                            RichText::new(lang.text("Save settings", "保存设置"))
                                .color(egui::Color32::WHITE),
                        )
                        .fill(ACCENT)
                        .min_size(egui::vec2(112.0, 34.0)),
                    )
                    .clicked()
                {
                    self.save_settings();
                }
                if ui
                    .add_enabled(
                        can_discard,
                        egui::Button::new(lang.text("Discard changes", "放弃修改"))
                            .min_size(egui::vec2(124.0, 34.0)),
                    )
                    .clicked()
                {
                    self.discard_changes();
                }
            });
        });
        if let Some(reason) = blocker {
            hint(ui, reason.get(lang));
        }
    }

    fn draw_general_settings(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        let mut language_preference = self.language_preference;
        settings_card(
            ui,
            lang,
            lang.text("Interface language", "界面语言"),
            false,
            |ui| {
                ui.add_enabled_ui(!self.closing, |ui| {
                    let label = ui.label(lang.text("Language", "语言"));
                    egui::ComboBox::from_id_salt("settings-interface-language")
                        .selected_text(match language_preference {
                            LanguagePreference::System => {
                                lang.text("System (default)", "跟随系统（默认）")
                            }
                            LanguagePreference::English => "English",
                            LanguagePreference::Chinese => "简体中文",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut language_preference,
                                LanguagePreference::System,
                                lang.text("System (default)", "跟随系统（默认）"),
                            );
                            ui.selectable_value(
                                &mut language_preference,
                                LanguagePreference::English,
                                "English",
                            );
                            ui.selectable_value(
                                &mut language_preference,
                                LanguagePreference::Chinese,
                                "简体中文",
                            );
                        })
                        .response
                        .labelled_by(label.id);
                });
                hint(
                ui,
                lang.text(
                    "Changes apply immediately and are saved separately on normal exit. Service settings are unchanged and services are not restarted.",
                    "立即切换，正常关闭时单独保存；不修改服务配置，也不会重启服务。",
                ),
            );
            },
        );
        if language_preference != self.language_preference {
            self.set_language_preference(language_preference);
        }
        ui.add_space(12.0);
        let editable = self.edits_allowed();
        let can_pick = editable && self.picking.is_none();
        let mut command = None;
        settings_card(
            ui,
            lang,
            lang.text("Sharing and state", "共享与状态"),
            false,
            |ui| {
                directory_field(
                    ui,
                    "shared-root",
                    lang.text("Shared directory", "共享目录"),
                    &mut self.form.root,
                    lang.text("Browse…", "选择共享目录"),
                    editable,
                    can_pick,
                    || command = Some(DirectoryTarget::Root),
                );
                hint(
                ui,
                lang.text(
                    "HTTP, TFTP and NFSv3 share files in this directory. HTTP and TFTP uploads are disabled by default; NFSv3 is always read-only.",
                    "HTTP、TFTP 和 NFSv3 共享此目录中的文件。HTTP 与 TFTP 上传默认关闭；NFSv3 始终只读。",
                ),
            );
                ui.add_space(16.0);
                directory_field(
                    ui,
                    "persistent-state",
                    lang.text("Persistent state directory", "持久状态目录"),
                    &mut self.form.state_dir,
                    lang.text("Browse…", "选择状态目录"),
                    editable,
                    can_pick,
                    || command = Some(DirectoryTarget::State),
                );
                hint(
                ui,
                lang.text(
                    "Stores persistent state such as NFS file handles; this directory is not shared with clients.",
                    "保存 NFS 文件句柄等持久状态，不是对外共享的文件目录。",
                ),
            );
                hint(
                ui,
                lang.text(
                    "Must be outside the shared directory. Use a different state directory when changing the shared directory.",
                    "必须位于共享目录之外；更换共享目录时，请使用另一持久状态目录。",
                ),
            );
            },
        );
        if let Some(target) = command {
            let initial = match target {
                DirectoryTarget::Root => &self.form.root,
                DirectoryTarget::State => &self.form.state_dir,
            };
            self.request(Command::PickDirectory(target, PathBuf::from(initial), lang));
        }
        ui.add_space(12.0);
        egui::CollapsingHeader::new(lang.text(
            "CLI default protocols (filemux serve only)",
            "CLI 默认启用协议（仅 filemux serve）",
        ))
            .id_salt("settings-cli-defaults")
            .show(ui, |ui| {
                hint(
                    ui,
                    lang.text(
                        "These options only affect filemux serve, not GUI service controls. Use the start and stop buttons on the Services page.",
                        "这些选项仅影响 filemux serve，不是 GUI 服务开关。GUI 请使用“服务”页的启动与停止按钮。",
                    ),
                );
                ui.add_enabled_ui(editable, |ui| {
                    ui.push_id("cli-default-protocols", |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.form.config.http.enabled, "HTTP");
                            ui.checkbox(&mut self.form.config.tftp.enabled, "TFTP");
                            ui.checkbox(&mut self.form.config.nfs.enabled, "NFSv3");
                        });
                    });
                });
            });
    }

    fn draw_http_settings(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        let editable = self.edits_allowed();
        settings_card(ui, lang, "HTTP", false, |ui| {
            ui.add_enabled_ui(editable, |ui| {
                bind_fields(
                    ui,
                    lang,
                    "http",
                    &mut self.form.http_ip,
                    &mut self.form.config.http.bind,
                );
                ui.add_space(12.0);
                ui.push_id("http-allow-upload", |ui| {
                    ui.checkbox(
                        &mut self.form.config.http.allow_upload,
                        lang.text("Allow HTTP uploads", "允许 HTTP 上传"),
                    );
                });
            });
            ui.add_space(8.0);
            hint(
                ui,
                lang.text(
                    "Uploads are disabled by default. When enabled, clients can create new files but cannot overwrite existing files.",
                    "上传默认关闭。启用后只允许新建文件，不覆盖已有文件。",
                ),
            );
            hint(
                ui,
                &format!(
                    "{}: {} {}",
                    lang.text("Maximum file size", "单文件大小上限"),
                    self.form.config.http.max_upload_bytes,
                    lang.text("bytes", "字节"),
                ),
            );
        });
    }

    fn draw_tftp_settings(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        let editable = self.edits_allowed();
        settings_card(ui, lang, "TFTP", false, |ui| {
            ui.add_enabled_ui(editable, |ui| {
                bind_fields(
                    ui,
                    lang,
                    "tftp",
                    &mut self.form.tftp_ip,
                    &mut self.form.config.tftp.bind,
                );
                ui.add_space(12.0);
                ui.push_id("tftp-allow-upload", |ui| {
                    ui.checkbox(
                        &mut self.form.config.tftp.allow_upload,
                        lang.text("Allow TFTP uploads", "允许 TFTP 上传"),
                    );
                });
            });
            hint(
                ui,
                lang.text(
                    "Uploads are disabled by default. When enabled, clients can create new files but cannot overwrite existing files.",
                    "上传默认关闭。启用后只允许新建文件，不覆盖已有文件。",
                ),
            );
        });
    }

    fn draw_nfs_settings(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        let editable = self.edits_allowed();
        settings_card(ui, lang, "NFSv3", true, |ui| {
            ui.add_enabled_ui(editable, |ui| {
                bind_fields(
                    ui,
                    lang,
                    "nfs",
                    &mut self.form.nfs_ip,
                    &mut self.form.config.nfs.bind,
                );
                ui.add_space(12.0);
                let label = ui.label(lang.text("Export path", "导出路径"));
                ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::TextEdit::singleline(&mut self.form.config.nfs.export)
                        .id(egui::Id::new("nfs-export-path")),
                )
                .labelled_by(label.id);
            });
            hint(
                ui,
                lang.text(
                    "The export name used by clients when mounting. Must start with / and maps to the shared directory under General. NFSv3 is always read-only.",
                    "客户端挂载时使用的导出名，以 / 开头；对应“通用”中的共享目录。NFSv3 始终只读。",
                ),
            );
        });
        ui.add_space(14.0);
        settings_card(
            ui,
            lang,
            lang.text("RPC port discovery", "RPC 端口发现"),
            false,
            |ui| {
                ui.add_enabled_ui(editable, |ui| {
                    ui.push_id("nfs-rpc-discovery-enabled", |ui| {
                        ui.checkbox(
                            &mut self.form.rpcbind_enabled,
                            lang.text("Enable RPC port discovery", "启用 RPC 端口发现"),
                        );
                    });
                    ui.add_space(8.0);
                    let label =
                        ui.label(lang.text("Listen address (IP:port)", "监听地址（IP:端口）"));
                    ui.add_enabled_ui(self.form.rpcbind_enabled, |ui| {
                        ui.add_sized(
                            [ui.available_width(), 30.0],
                            egui::TextEdit::singleline(&mut self.form.rpcbind)
                                .id(egui::Id::new("nfs-rpc-discovery-address"))
                                .hint_text(
                                    lang.text("e.g. 192.168.10.20:111", "例如 192.168.10.20:111"),
                                ),
                        )
                        .labelled_by(label.id);
                    });
                });
                if !self.form.rpcbind_enabled {
                    hint(
                        ui,
                        lang.text(
                            "Enable RPC port discovery to edit the listen address.",
                            "启用 RPC 端口发现后才能编辑监听地址。",
                        ),
                    );
                }
                hint(
                ui,
                lang.text(
                    "U-Boot usually needs discovery on UDP port 111. Change this with care. Low ports may require system privileges; firewall rules are not changed automatically.",
                    "U-Boot 通常需要 UDP 111 端口发现，请谨慎修改。低端口可能需要系统权限；不会自动修改防火墙。",
                ),
            );
            },
        );
    }

    fn draw_pxe_settings(&mut self, ui: &mut egui::Ui) {
        let lang = self.language();
        let editable = self.edits_allowed();
        settings_card(ui, lang, lang.text("PXE boot", "PXE 启动"), false, |ui| {
            ui.add_enabled_ui(editable, |ui| {
                ui.checkbox(
                    &mut self.form.config.pxe.enabled,
                    lang.text("Enable PXE menu", "启用 PXE 菜单"),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(
                        &mut self.form.config.pxe.mode,
                        PxeMode::ExternalDhcp,
                        lang.text("External DHCP", "外部 DHCP"),
                    );
                    ui.selectable_value(
                        &mut self.form.config.pxe.mode,
                        PxeMode::ProxyDhcp,
                        "ProxyDHCP",
                    );
                });
            });
            hint(ui, lang.text(
                "Both modes need an existing DHCP server to assign IP addresses. Filemux does not change router settings.",
                "两种模式都需要原 DHCP 分配 IP；Filemux 不修改路由器设置。"));
            if self.form.config.pxe.mode == PxeMode::ProxyDhcp {
                hint(ui, lang.text(
                    "ProxyDHCP only supplies boot information on the selected IPv4 interface. UDP 67/4011 must be free; privileges may be required. Firewall rules are not changed.",
                    "ProxyDHCP 不分配 IP，只向所选 IPv4 所属接口提供启动信息。UDP 67/4011 须未被占用，可能需要权限；不会修改防火墙。"));
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(editable, |ui| {
                pxe_field(
                    ui,
                    lang.text("Server IPv4", "服务 IPv4"),
                    &mut self.form.pxe_server_ip,
                );
            });
            hint(ui, lang.text(
                "Choose a local IPv4 reachable by clients. HTTP must listen there or on 0.0.0.0 with a fixed port; TFTP must use port 69.",
                "填写客户端可访问的本机 IPv4；HTTP 须监听该 IP 或 0.0.0.0 并使用固定端口，TFTP 须使用 69 端口。"));
        });
        ui.add_space(14.0);
        settings_card(
            ui,
            lang,
            lang.text("Boot programs", "启动程序"),
            false,
            |ui| {
                ui.add_enabled_ui(editable, |ui| {
                    pxe_field(
                        ui,
                        lang.text("BIOS x86 iPXE", "BIOS x86 iPXE 文件"),
                        &mut self.form.config.pxe.bios_bootfile,
                    );
                    pxe_field(
                        ui,
                        lang.text("UEFI x64 iPXE", "UEFI x64 iPXE 文件"),
                        &mut self.form.config.pxe.uefi_x64_bootfile,
                    );
                    pxe_field(
                        ui,
                        lang.text("wimboot (Windows PE only)", "wimboot（仅 Windows PE 需要）"),
                        &mut self.form.config.pxe.wimboot,
                    );
                });
                hint(ui, lang.text("All file paths are relative to the shared directory; use /. Parent directories must already exist.", "所有文件路径均相对于共享目录，使用 /；父目录须已存在。"));
                let can_download = editable
                    && self.saved_form.is_some()
                    && !self.dirty()
                    && self.operation.is_none()
                    && self.picking.is_none();
                for tool in [BootTool::Ipxe, BootTool::Wimboot] {
                    let spec = tool.spec();
                    ui.separator();
                    let title = match tool {
                        BootTool::Ipxe => "iPXE",
                        BootTool::Wimboot => "wimboot",
                    };
                    let targets = match tool {
                        BootTool::Ipxe => format!(
                            "{} · {}",
                            self.form.config.pxe.bios_bootfile,
                            self.form.config.pxe.uefi_x64_bootfile
                        ),
                        BootTool::Wimboot => self.form.config.pxe.wimboot.clone(),
                    };
                    ui.label(format!("{title} {} · {targets}", spec.version));
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(
                                can_download,
                                egui::Button::new(format!(
                                    "{} {title}",
                                    lang.text("Download", "下载")
                                )),
                            )
                            .on_disabled_hover_text(lang.text(
                                "Save changes and wait for other operations before downloading.",
                                "请先保存修改，并等待其他操作完成后下载。",
                            ))
                            .clicked()
                        {
                            self.request(Command::DownloadBootTool(tool));
                        }
                        ui.hyperlink_to(lang.text("Official release", "官方发布"), spec.url);
                        ui.hyperlink_to(lang.text("Source", "源码"), spec.source_url);
                        ui.hyperlink_to(lang.text("License", "许可"), spec.license_url);
                    });
                }
                hint(ui, lang.text(
                "Downloads are optional and never overwrite files or enable PXE. Linux files and boot.wim must be supplied by you; Windows images are not downloaded.",
                "下载为可选操作，不覆盖文件、不自动启用 PXE。Linux 文件和 boot.wim 需自行准备，不会下载 Windows 镜像。"));
                if let Some(result) = &self.pxe_download {
                    match result {
                        Ok(report) => {
                            for path in &report.saved {
                                ui.label(format!("{}: {path}", lang.text("Saved", "已保存")));
                            }
                            for path in &report.skipped {
                                ui.label(format!(
                                    "{}: {path}",
                                    lang.text("Already exists; not overwritten", "已存在，未覆盖")
                                ));
                            }
                        }
                        Err(error) => {
                            ui.colored_label(
                                RED,
                                lang.text("Download failed or cancelled", "下载失败或已取消"),
                            );
                            ui.add(egui::Label::new(error).wrap().selectable(true));
                        }
                    }
                }
            },
        );
        ui.add_space(14.0);
        settings_card(
            ui,
            lang,
            lang.text("Boot menu", "启动菜单"),
            false,
            |ui| {
                let mut action = None;
                ui.add_enabled_ui(editable, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let label =
                            ui.label(lang.text("Selection timeout (seconds)", "选择超时（秒）"));
                        ui.add(
                            egui::DragValue::new(&mut self.form.config.pxe.timeout_secs)
                                .range(0..=u16::MAX),
                        )
                        .labelled_by(label.id);
                        hint(
                            ui,
                            lang.text("0 waits for manual selection", "0 表示等待人工选择"),
                        );
                    });
                    let pxe = &mut self.form.config.pxe;
                    let count = pxe.entries.len();
                    for (index, entry) in pxe.entries.iter_mut().enumerate() {
                        ui.push_id(("pxe-entry", index), |ui| {
                            ui.separator();
                            let selected = pxe
                                .default_entry
                                .as_deref()
                                .map(|id| id == entry.id())
                                .unwrap_or(index == 0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(match entry {
                                    PxeEntry::Linux { .. } => "Linux",
                                    PxeEntry::Winpe { .. } => "Windows PE",
                                    PxeEntry::Chain { .. } => "iPXE",
                                });
                                if ui
                                    .radio(selected, lang.text("Default", "默认启动"))
                                    .clicked()
                                {
                                    pxe.default_entry = Some(entry.id().into());
                                }
                                if ui
                                    .add_enabled(
                                        index > 0,
                                        egui::Button::new(lang.text("Move up", "上移")),
                                    )
                                    .clicked()
                                {
                                    action = Some((index, -1));
                                }
                                if ui
                                    .add_enabled(
                                        index + 1 < count,
                                        egui::Button::new(lang.text("Move down", "下移")),
                                    )
                                    .clicked()
                                {
                                    action = Some((index, 1));
                                }
                                if ui.button(lang.text("Delete", "删除")).clicked() {
                                    action = Some((index, 0));
                                }
                            });
                            let (id, title) = match entry {
                                PxeEntry::Linux { id, title, .. }
                                | PxeEntry::Winpe { id, title, .. }
                                | PxeEntry::Chain { id, title, .. } => (id, title),
                            };
                            pxe_field(ui, "ID", id);
                            if selected && pxe.default_entry.is_some() {
                                pxe.default_entry = Some(id.clone());
                            }
                            pxe_field(ui, lang.text("Title", "标题"), title);
                            match entry {
                                PxeEntry::Linux {
                                    kernel,
                                    initrd,
                                    args,
                                    ..
                                } => {
                                    pxe_field(ui, lang.text("Kernel file", "内核文件"), kernel);
                                    pxe_field(ui, lang.text("Initrd file", "initrd 文件"), initrd);
                                    pxe_field(ui, lang.text("Kernel arguments", "内核参数"), args);
                                }
                                PxeEntry::Winpe { boot_wim, .. } => {
                                    pxe_field(ui, "boot.wim", boot_wim)
                                }
                                PxeEntry::Chain { script, .. } => pxe_field(
                                    ui,
                                    lang.text("Local iPXE script", "本地 iPXE 脚本"),
                                    script,
                                ),
                            }
                        });
                    }
                    if let Some((index, direction)) = action {
                        match direction {
                            -1 => pxe.entries.swap(index, index - 1),
                            1 => pxe.entries.swap(index, index + 1),
                            _ => {
                                if pxe.default_entry.as_deref() == Some(pxe.entries[index].id()) {
                                    pxe.default_entry = None;
                                }
                                pxe.entries.remove(index);
                            }
                        }
                    }
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        for (prefix, label) in [
                            ("linux", lang.text("Add Linux", "添加 Linux")),
                            ("winpe", lang.text("Add Windows PE", "添加 Windows PE")),
                            ("chain", lang.text("Add iPXE script", "添加 iPXE 脚本")),
                        ] {
                            if ui.button(label).clicked() {
                                let id = (1..)
                                    .map(|number| format!("{prefix}{number}"))
                                    .find(|id| !pxe.entries.iter().any(|entry| entry.id() == id))
                                    .expect("available ID");
                                pxe.entries.push(match prefix {
                                    "linux" => PxeEntry::Linux {
                                        id,
                                        title: "Linux".into(),
                                        kernel: String::new(),
                                        initrd: String::new(),
                                        args: String::new(),
                                    },
                                    "winpe" => PxeEntry::Winpe {
                                        id,
                                        title: "Windows PE".into(),
                                        boot_wim: String::new(),
                                    },
                                    _ => PxeEntry::Chain {
                                        id,
                                        title: "iPXE".into(),
                                        script: String::new(),
                                    },
                                });
                            }
                        }
                    });
                });
                hint(ui, lang.text(
                "IDs must be unique. Without an explicit default, the first entry is used. Failed boots return to a menu without a countdown.",
                "ID 不可重复；未指定默认项时使用第一项。启动失败后返回菜单，不再次倒计时。"));
            },
        );
        ui.add_space(14.0);
        settings_card(
            ui,
            lang,
            lang.text("Files and DHCP settings", "文件检查与 DHCP 设置"),
            false,
            |ui| {
                let pxe = &self.form.config.pxe;
                ui.add(
                    egui::Label::new(format!(
                        "{}: http://{}:{}/.filemux-pxe/menu.ipxe",
                        lang.text("Menu URL", "菜单 URL"),
                        self.form.pxe_server_ip,
                        self.form.config.http.bind.port()
                    ))
                    .wrap()
                    .selectable(true),
                );
                ui.add(
                    egui::Label::new(format!(
                        "next-server: {} · BIOS: {} · UEFI x64: {}",
                        self.form.pxe_server_ip, pxe.bios_bootfile, pxe.uefi_x64_bootfile
                    ))
                    .wrap()
                    .selectable(true),
                );
                hint(ui, lang.text(
                "External DHCP: use option 93 to select BIOS x86 or UEFI x64 on the first request. For clients already in iPXE, return the menu URL instead of a binary.",
                "外部 DHCP：首次请求按 option 93 区分 BIOS x86 与 UEFI x64；已进入 iPXE 的请求返回菜单 URL，不要再次返回二进制文件。"));
                let can_check = editable && self.operation.is_none() && self.picking.is_none();
                if ui
                    .add_enabled(
                        can_check,
                        egui::Button::new(lang.text("Check boot files", "检查启动文件")),
                    )
                    .clicked()
                {
                    match self.form.build() {
                        Ok(config) => self.request(Command::CheckPxe(config)),
                        Err(error) => self.notice = Some(error),
                    }
                }
                match &self.pxe_check {
                    None => {
                        ui.label(if self.pxe_task == Some(PxeTask::Check) {
                            lang.text("Checking files…", "正在检查文件…")
                        } else {
                            lang.text("Not checked for these settings", "当前设置尚未检查")
                        });
                    }
                    Some(Err(error)) => {
                        ui.colored_label(RED, lang.text("File check failed", "文件检查失败"));
                        ui.add(egui::Label::new(error).wrap().selectable(true));
                    }
                    Some(Ok(report)) => {
                        ui.colored_label(
                            if report.is_ok() { GREEN } else { RED },
                            if report.is_ok() {
                                lang.text("File check passed", "文件检查通过")
                            } else {
                                lang.text("File check failed", "文件检查失败")
                            },
                        );
                        for resource in &report.resources {
                            ui.add(
                                egui::Label::new(format!(
                                    "{} · {} — {}",
                                    resource.name,
                                    resource.path,
                                    resource
                                        .error
                                        .as_deref()
                                        .unwrap_or(lang.text("File exists", "文件存在"))
                                ))
                                .wrap()
                                .selectable(true),
                            );
                        }
                    }
                }
                hint(ui, lang.text("This only checks paths and non-empty regular files, not whether an image can boot. Files can change after checking.", "这里只检查路径与非空常规文件，不证明镜像可启动；检查后文件仍可能变化。"));
                hint(ui, lang.text("BIOS x86 / UEFI x64, one IPv4 interface and subnet only. No relay, IPv6 or Secure Boot. Loopback addresses are for local software checks only.", "仅支持 BIOS x86／UEFI x64、单 IPv4 接口和同一子网；不支持 relay、IPv6 或 Secure Boot。loopback 仅供本机软件验证。"));
            },
        );
    }
}

fn settings_card(
    ui: &mut egui::Ui,
    lang: Language,
    title: &str,
    readonly: bool,
    body: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::group(ui.style())
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).heading().strong());
                if readonly {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::Frame::group(ui.style())
                            .stroke(Stroke::new(1.0_f32, BORDER))
                            .show(ui, |ui| {
                                ui.label(lang.text("Read-only", "只读"));
                            });
                    });
                }
            });
            ui.separator();
            ui.add_space(8.0);
            body(ui);
        });
}

fn bind_fields(
    ui: &mut egui::Ui,
    lang: Language,
    id: &'static str,
    ip: &mut String,
    address: &mut SocketAddr,
) {
    let port_width = 112.0;
    let ip_width = (ui.available_width() - port_width - ui.spacing().item_spacing.x).max(80.0);
    egui::Grid::new(("settings-listener", id))
        .num_columns(2)
        .spacing([ui.spacing().item_spacing.x, 5.0])
        .show(ui, |ui| {
            let ip_label = ui.label(lang.text("Listen IP", "监听 IP"));
            let port_label = ui.label(lang.text("Port", "端口"));
            ui.end_row();
            ui.add_sized(
                [ip_width, 30.0],
                egui::TextEdit::singleline(ip).id(egui::Id::new(("settings-listener-ip", id))),
            )
            .labelled_by(ip_label.id);
            let mut port = address.port();
            let response = ui.push_id(("settings-listener-port", id), |ui| {
                ui.add_sized(
                    [port_width, 30.0],
                    egui::DragValue::new(&mut port).range(0..=u16::MAX),
                )
                .labelled_by(port_label.id)
            });
            if response.inner.changed() {
                address.set_port(port);
            }
            ui.end_row();
        });
}

fn directory_field(
    ui: &mut egui::Ui,
    id: &'static str,
    title: &str,
    value: &mut String,
    button: &str,
    editable: bool,
    can_pick: bool,
    picked: impl FnOnce(),
) {
    let label = ui.label(title);
    ui.horizontal(|ui| {
        let button_width = 116.0;
        let text_width =
            (ui.available_width() - button_width - ui.spacing().item_spacing.x).max(80.0);
        ui.add_enabled_ui(editable, |ui| {
            ui.add_sized(
                [text_width, 32.0],
                egui::TextEdit::singleline(value).id(egui::Id::new(("settings-directory", id))),
            )
            .labelled_by(label.id);
        });
        let response = ui.push_id(("settings-directory-picker", id), |ui| {
            ui.add_enabled(
                can_pick,
                egui::Button::new(button).min_size(egui::vec2(button_width, 32.0)),
            )
        });
        if response.inner.clicked() {
            picked();
        }
    });
}

fn pxe_field(ui: &mut egui::Ui, title: &str, value: &mut String) {
    let label = ui.label(title);
    ui.add_sized(
        [ui.available_width(), 30.0],
        egui::TextEdit::singleline(value),
    )
    .labelled_by(label.id);
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(MUTED));
}
