use anyhow::{Context, Result, bail};
use eframe::egui;
use filemux_core::{Config, Event, Protocol, ServiceState};
use filemux_engine::{Engine, ServiceSnapshot};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const MAX_LOGS: usize = 500;

#[derive(Clone, Copy)]
pub enum DirectoryTarget {
    Root,
    State,
}

pub enum Command {
    Save(Config),
    Start(Protocol),
    Stop(Protocol),
    StopAll,
    PickDirectory(DirectoryTarget, PathBuf),
}

pub struct Notice {
    pub text: String,
    pub is_error: bool,
}

pub struct UiState {
    pub snapshots: Vec<ServiceSnapshot>,
    pub busy: bool,
    pub notice: Option<Notice>,
    pub saved: Option<Config>,
    pub picked: Option<(DirectoryTarget, PathBuf)>,
    pub logs: VecDeque<Event>,
    pub settings_in_use: bool,
    pub stopped: bool,
}

pub struct Worker {
    pub state: Arc<Mutex<UiState>>,
    commands: mpsc::Sender<Command>,
    cancel: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(
        config: Config,
        has_saved_config: bool,
        config_path: PathBuf,
        ctx: egui::Context,
    ) -> Result<Self> {
        let state = Arc::new(Mutex::new(UiState {
            snapshots: Protocol::ALL
                .into_iter()
                .map(|protocol| ServiceSnapshot {
                    protocol,
                    state: ServiceState::Stopped,
                    endpoints: Vec::new(),
                    error: None,
                })
                .collect(),
            busy: false,
            notice: None,
            saved: None,
            picked: None,
            logs: VecDeque::new(),
            settings_in_use: false,
            stopped: false,
        }));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .context("无法创建服务运行时")?;
        let (commands, mut receiver) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let shared = Arc::clone(&state);
        let thread = std::thread::Builder::new().name("filemux-engine".into()).spawn(move || {
            runtime.block_on(async {
                let mut engine = Engine::new(config.clone());
                let mut events = engine.subscribe();
                let mut saved_config = has_saved_config.then_some(config);
                let mut health = tokio::time::interval(Duration::from_millis(500));
                loop {
                    tokio::select! {
                        biased;
                        _ = stop.cancelled() => break,
                        command = receiver.recv() => {
                            let Some(command) = command else { break };
                            {
                                let mut ui = shared.lock().expect("UI state mutex");
                                match &command {
                                    Command::Start(protocol) => {
                                        if let Some(snapshot) = ui.snapshots.iter_mut().find(|s| s.protocol == *protocol) {
                                            snapshot.state = ServiceState::Starting;
                                        }
                                    }
                                    Command::Stop(protocol) => {
                                        if let Some(snapshot) = ui.snapshots.iter_mut().find(|s| s.protocol == *protocol) {
                                            snapshot.state = ServiceState::Stopping;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            ctx.request_repaint();
                            let result: Result<String> = async {
                                match command {
                                    Command::Save(config) => {
                                        let candidate = config.clone();
                                        let path = config_path.clone();
                                        tokio::task::spawn_blocking(move || config.save(path)).await.context("保存设置任务异常")??;
                                        saved_config = Some(candidate.clone());
                                        shared.lock().expect("UI state mutex").saved = Some(candidate);
                                        Ok("设置已保存".into())
                                    }
                                    Command::Start(protocol) => {
                                        let config = saved_config.clone().context("请先保存设置，再启动服务")?;
                                        engine.configure(config)?;
                                        engine.start(protocol).await?;
                                        Ok(format!("{protocol} 已启动"))
                                    }
                                    Command::Stop(protocol) => {
                                        engine.stop(protocol).await?;
                                        Ok(format!("{protocol} 已停止"))
                                    }
                                    Command::StopAll => {
                                        engine.stop_all().await?;
                                        Ok("所有服务已停止".into())
                                    }
                                    Command::PickDirectory(target, initial) => {
                                        let title = match target { DirectoryTarget::Root => "选择共享目录", DirectoryTarget::State => "选择持久状态目录" };
                                        let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
                                        if initial.is_dir() { dialog = dialog.set_directory(initial); }
                                        if let Some(folder) = dialog.pick_folder().await {
                                            shared.lock().expect("UI state mutex").picked = Some((target, folder.path().to_path_buf()));
                                            Ok("目录已选择，尚未保存".into())
                                        } else { Ok("未选择目录".into()) }
                                    }
                                }
                            }.await;
                            let mut ui = shared.lock().expect("UI state mutex");
                            ui.snapshots = engine.snapshot();
                            ui.settings_in_use = saved_config.as_ref().is_some_and(|config| config == engine.config());
                            ui.busy = false;
                            ui.notice = Some(match result {
                                Ok(text) => Notice { text, is_error: false },
                                Err(error) => Notice { text: format!("{error:#}"), is_error: true },
                            });
                            drop(ui);
                            ctx.request_repaint();
                        }
                        _ = health.tick() => {
                            if let Err(error) = engine.check_health().await {
                                let mut ui = shared.lock().expect("UI state mutex");
                                ui.snapshots = engine.snapshot();
                                ui.notice = Some(Notice { text: format!("{error:#}"), is_error: true });
                                drop(ui);
                                ctx.request_repaint();
                            }
                        }
                        event = events.recv() => {
                            if let Ok(event) = event {
                                let mut ui = shared.lock().expect("UI state mutex");
                                if ui.logs.len() == MAX_LOGS { ui.logs.pop_front(); }
                                ui.logs.push_back(event);
                                drop(ui);
                                ctx.request_repaint();
                            }
                        }
                    }
                }
                let result = engine.stop_all().await;
                let mut ui = shared.lock().expect("UI state mutex");
                ui.snapshots = engine.snapshot();
                ui.busy = false;
                ui.stopped = true;
                if let Err(error) = result {
                    eprintln!("停止 GUI 内部服务时发生错误：{error:#}");
                    ui.notice = Some(Notice { text: format!("停止服务时发生错误：{error:#}"), is_error: true });
                }
                drop(ui);
                ctx.request_repaint();
            });
            runtime.shutdown_timeout(Duration::from_secs(5));
        }).context("无法创建后台服务线程")?;
        Ok(Self {
            state,
            commands,
            cancel,
            thread: Some(thread),
        })
    }

    pub fn request(&self, command: Command) -> Result<()> {
        {
            let mut ui = self.state.lock().expect("UI state mutex");
            if ui.busy {
                bail!("上一个操作尚未完成");
            }
            ui.busy = true;
        }
        if let Err(error) = self.commands.try_send(command) {
            self.state.lock().expect("UI state mutex").busy = false;
            bail!("无法发送操作到服务线程：{error}");
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
                eprintln!("后台服务线程异常退出");
            }
        }
    }
}
