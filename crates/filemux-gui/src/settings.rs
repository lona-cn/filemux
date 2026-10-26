use crate::i18n::{Language, LanguagePreference};
use crate::worker::{Command, DirectoryTarget};
use crate::{ACCENT, BORDER, FilemuxApp, GREEN, MUTED, PANEL, RED, SettingsTab};
use eframe::egui::{self, RichText, Stroke};
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

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(MUTED));
}
