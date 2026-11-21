//! mihomo (Clash.Meta) 核心的后台运行服务。
//!
//! 设置保存在 [`crate::config::MihomoConfig`]；本模块只负责把驱动
//! (`mihomo_driver`) 的进程监督与控制器 API 包装成一个可启动、停止、
//! 重新加载并持续汇报状态的后台服务。

use crate::config::MihomoConfig;
pub mod management;
use anyhow::Result;
use async_channel::{Receiver, Sender};
use lurker_core_contract::core_process::CoreProcessStartContext;
use lurker_core_contract::lifecycle::CoreLifecycle;
use lurker_core_ports::core_process::{CoreProcess, CoreReadiness};
use mihomo_driver::driver::{ControllerReadiness, ProcessCoreController};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use tokio::sync::watch;

const READINESS_TIMEOUT: Duration = Duration::from_secs(20);
const READINESS_INTERVAL: Duration = Duration::from_millis(300);
const COMMAND_CAPACITY: usize = 16;
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

/// 核心的生命周期状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MihomoStatus {
    #[default]
    Stopped,
    Starting,
    Running {
        version: Option<String>,
        pid: Option<u32>,
    },
    Stopping,
}

impl MihomoStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Starting | Self::Stopping)
    }
}

/// 后台服务对外汇报的完整状态：生命周期加上最近一次操作的错误或提示。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MihomoState {
    pub status: MihomoStatus,
    pub error: Option<String>,
    pub notice: Option<String>,
}

enum Command {
    Start(MihomoConfig),
    Adopt(MihomoConfig),
    Stop,
    Reload,
    Refresh,
    Shutdown,
}

/// 持有 mihomo 工作线程的句柄。
///
/// 命令通过有界队列发送，状态通过 [`tokio::sync::watch`] 广播；
/// UI 只需持有该句柄并在状态变化时刷新。
pub struct MihomoService {
    commands: Sender<Command>,
    state: watch::Receiver<MihomoState>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    done: Mutex<Option<mpsc::Receiver<()>>>,
    shutdown_requested: Arc<AtomicBool>,
}

impl MihomoService {
    /// 创建后台服务。`home` 用于驱动保存 PID 记录和核心日志。
    pub fn new(home: PathBuf) -> Self {
        let (commands, command_rx) = async_channel::bounded(COMMAND_CAPACITY);
        let (state_tx, state_rx) = watch::channel(MihomoState::default());
        let (done_tx, done_rx) = mpsc::channel();
        let failure_tx = state_tx.clone();
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown_requested.clone();
        let worker = thread::Builder::new()
            .name("moin-mihomo".into())
            .spawn(move || {
                run_worker(home, command_rx, state_tx, worker_shutdown);
                let _ = done_tx.send(());
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                let _ = failure_tx.send(MihomoState {
                    status: MihomoStatus::Stopped,
                    error: Some(format!("无法创建 mihomo 工作线程 · {error}")),
                    notice: None,
                });
                None
            }
        };
        let done = worker.as_ref().map(|_| done_rx);
        Self {
            commands,
            state: state_rx,
            worker: Mutex::new(worker),
            done: Mutex::new(done),
            shutdown_requested,
        }
    }

    pub fn state(&self) -> MihomoState {
        self.state.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<MihomoState> {
        self.state.clone()
    }

    pub fn start(&self, config: MihomoConfig) -> Result<()> {
        self.send(Command::Start(config.normalized()))
    }

    pub fn stop(&self) -> Result<()> {
        self.send(Command::Stop)
    }

    /// Record a configuration already loaded successfully through the controller.
    pub fn adopt(&self, config: MihomoConfig) -> Result<()> {
        self.send(Command::Adopt(config))
    }

    pub fn reload(&self) -> Result<()> {
        self.send(Command::Reload)
    }

    pub fn refresh(&self) -> Result<()> {
        self.send(Command::Refresh)
    }

    /// 请求后台线程停止核心；应用退出时使用，不等待完成。
    pub fn shutdown(&self) {
        if !self.shutdown_requested.swap(true, Ordering::SeqCst) {
            let _ = self.commands.try_send(Command::Shutdown);
        }
    }

    fn send(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|error| anyhow::anyhow!("mihomo 后台服务不可用 · {error}"))
    }
}

impl Drop for MihomoService {
    fn drop(&mut self) {
        if !self.shutdown_requested.swap(true, Ordering::SeqCst) {
            let _ = self.commands.send_blocking(Command::Shutdown);
        }
        // The worker may be in the middle of a start with a 20 s readiness
        // deadline; never block application exit for that long.
        let stopped = self
            .done
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .is_some_and(|done| done.recv_timeout(SHUTDOWN_WAIT).is_ok());
        if stopped
            && let Some(worker) = self
                .worker
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
        {
            let _ = worker.join();
        }
    }
}

fn run_worker(
    home: PathBuf,
    commands: Receiver<Command>,
    state: watch::Sender<MihomoState>,
    shutdown: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = state.send(MihomoState {
                status: MihomoStatus::Stopped,
                error: Some(format!("无法创建 mihomo 运行时 · {error}")),
                notice: None,
            });
            return;
        }
    };
    runtime.block_on(async move {
        let mut controller: Option<ProcessCoreController> = None;
        let mut running: Option<MihomoConfig> = None;
        while let Ok(command) = commands.recv().await {
            match command {
                Command::Start(config) => {
                    start_core(
                        &home,
                        config,
                        &mut controller,
                        &mut running,
                        &state,
                        &shutdown,
                    )
                    .await;
                }
                Command::Stop => stop_core(&mut controller, &mut running, &state).await,
                Command::Adopt(config) => {
                    if running.is_some() {
                        running = Some(config);
                    }
                }
                Command::Reload => reload_core(running.as_ref(), &state).await,
                Command::Refresh => {
                    refresh_core(&mut controller, &mut running, &state).await;
                }
                Command::Shutdown => {
                    if let Some(existing) = controller.take() {
                        set_status(&state, MihomoStatus::Stopping);
                        let _ = existing.stop().await;
                    }
                    let _ = state.send(MihomoState {
                        status: MihomoStatus::Stopped,
                        error: None,
                        notice: None,
                    });
                    break;
                }
            }
        }
        // The host dropped the service without a Shutdown command: never
        // leave the core behind.
        if let Some(existing) = controller.take() {
            let _ = existing.stop().await;
        }
    });
    runtime.shutdown_timeout(Duration::from_millis(500));
}

async fn start_core(
    home: &Path,
    config: MihomoConfig,
    controller: &mut Option<ProcessCoreController>,
    running: &mut Option<MihomoConfig>,
    state: &watch::Sender<MihomoState>,
    shutdown: &AtomicBool,
) {
    if shutdown.load(Ordering::SeqCst) {
        return;
    }
    if let Err(error) = config.validate_for_start() {
        report_error(state, None, format!("{error:#}"));
        return;
    }

    if let Some(existing) = controller.take() {
        match existing.status().await {
            Ok(CoreLifecycle::Stopped) => {
                *running = None;
            }
            Ok(_) if running.as_ref() == Some(&config) => {
                let version = probe_version(&config).await;
                let pid = existing.pid().await;
                *controller = Some(existing);
                report_ok(
                    state,
                    MihomoStatus::Running { version, pid },
                    "mihomo 核心已在运行",
                );
                return;
            }
            Ok(_) => {
                set_status(state, MihomoStatus::Stopping);
                if let Err(error) = existing.stop().await {
                    *controller = Some(existing);
                    report_error(
                        state,
                        None,
                        format!("切换配置前无法停止 mihomo 核心 · {error}"),
                    );
                    return;
                }
                *running = None;
            }
            Err(error) => {
                *controller = Some(existing);
                report_error(state, None, format!("无法读取 mihomo 进程状态 · {error}"));
                return;
            }
        }
    }

    let _ = state.send(MihomoState {
        status: MihomoStatus::Starting,
        error: None,
        notice: None,
    });
    let next = ProcessCoreController::with_home(
        config.binary_path.clone(),
        config.config_path.clone(),
        home.to_path_buf(),
    );
    let Some(identity) = next.core_identity() else {
        report_error(
            state,
            Some(MihomoStatus::Stopped),
            "mihomo 驱动没有提供核心标识",
        );
        return;
    };
    if let Err(error) = next
        .start(CoreProcessStartContext::standalone(identity))
        .await
    {
        report_error(
            state,
            Some(MihomoStatus::Stopped),
            format!("无法启动 mihomo 核心 · {error}"),
        );
        return;
    }

    let readiness = ControllerReadiness::new(config.controller.clone(), config.secret());
    match wait_until_ready(&next, &readiness, shutdown).await {
        Ok(()) => {
            let version = readiness
                .active_core_version()
                .await
                .ok()
                .flatten()
                .map(|value| value.raw().to_owned());
            let pid = next.pid().await;
            *controller = Some(next);
            *running = Some(config);
            report_ok(
                state,
                MihomoStatus::Running { version, pid },
                "mihomo 核心已启动",
            );
        }
        Err(error) => {
            let pid = next.pid().await;
            match next.stop().await {
                Ok(()) => report_error(state, Some(MihomoStatus::Stopped), format!("{error:#}")),
                Err(stop_error) => {
                    *controller = Some(next);
                    *running = Some(config);
                    report_error(
                        state,
                        Some(MihomoStatus::Running { version: None, pid }),
                        format!("{error:#}；同时无法停止该进程 · {stop_error}"),
                    );
                }
            }
        }
    }
}

async fn stop_core(
    controller: &mut Option<ProcessCoreController>,
    running: &mut Option<MihomoConfig>,
    state: &watch::Sender<MihomoState>,
) {
    let Some(existing) = controller.take() else {
        *running = None;
        report_ok(state, MihomoStatus::Stopped, "mihomo 核心未运行");
        return;
    };
    set_status(state, MihomoStatus::Stopping);
    match existing.stop().await {
        Ok(()) => {
            *running = None;
            report_ok(state, MihomoStatus::Stopped, "mihomo 核心已停止");
        }
        Err(error) => {
            let pid = existing.pid().await;
            let alive = matches!(existing.status().await, Ok(CoreLifecycle::Running));
            if alive {
                *controller = Some(existing);
                report_error(
                    state,
                    Some(MihomoStatus::Running { version: None, pid }),
                    format!("无法停止 mihomo 核心 · {error}"),
                );
            } else {
                *running = None;
                report_error(
                    state,
                    Some(MihomoStatus::Stopped),
                    format!("无法确认 mihomo 核心已停止 · {error}"),
                );
            }
        }
    }
}

async fn reload_core(running: Option<&MihomoConfig>, state: &watch::Sender<MihomoState>) {
    let Some(config) = running else {
        report_error(state, None, "mihomo 核心未运行，无法重新加载配置");
        return;
    };
    let config = config.clone();
    match tokio::task::spawn_blocking(move || {
        management::Controller::new(&config)?.reload(&config.config_path)
    })
    .await
    {
        Ok(result) => match result {
            Ok(()) => {
                let mut next = state.borrow().clone();
                next.error = None;
                next.notice = Some("已重新加载 mihomo 配置".into());
                let _ = state.send(next);
            }
            Err(error) => report_error(state, None, format!("mihomo 配置重新加载失败 · {error}")),
        },
        Err(error) => report_error(state, None, format!("mihomo 控制器任务失败 · {error}")),
    }
}

async fn refresh_core(
    controller: &mut Option<ProcessCoreController>,
    running: &mut Option<MihomoConfig>,
    state: &watch::Sender<MihomoState>,
) {
    let Some(existing) = controller.take() else {
        *running = None;
        report_ok(state, MihomoStatus::Stopped, "mihomo 核心未运行");
        return;
    };
    match existing.status().await {
        Ok(CoreLifecycle::Stopped) => {
            *running = None;
            report_ok(state, MihomoStatus::Stopped, "mihomo 核心已停止");
        }
        Ok(_) => {
            let pid = existing.pid().await;
            let version = match running.as_ref() {
                Some(config) => probe_version(config).await,
                None => None,
            };
            *controller = Some(existing);
            report_ok(
                state,
                MihomoStatus::Running { version, pid },
                "mihomo 状态已刷新",
            );
        }
        Err(error) => {
            *controller = Some(existing);
            report_error(state, None, format!("无法读取 mihomo 进程状态 · {error}"));
        }
    }
}

async fn wait_until_ready(
    controller: &ProcessCoreController,
    readiness: &ControllerReadiness,
    shutdown: &AtomicBool,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            anyhow::bail!("Moin 正在退出，已取消等待核心就绪");
        }
        match controller.status().await {
            Ok(CoreLifecycle::Stopped) => {
                anyhow::bail!("mihomo 进程在控制器就绪前退出");
            }
            Ok(_) => {}
            Err(error) => anyhow::bail!("无法读取 mihomo 进程状态 · {error}"),
        }
        match readiness.probe().await {
            Ok(_) => return Ok(()),
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    anyhow::bail!(
                        "等待 mihomo 控制器超时（{}s）· {error}",
                        READINESS_TIMEOUT.as_secs()
                    );
                }
                tokio::time::sleep(READINESS_INTERVAL).await;
            }
        }
    }
}

async fn probe_version(config: &MihomoConfig) -> Option<String> {
    ControllerReadiness::new(config.controller.clone(), config.secret())
        .active_core_version()
        .await
        .ok()
        .flatten()
        .map(|value| value.raw().to_owned())
}

fn set_status(state: &watch::Sender<MihomoState>, status: MihomoStatus) {
    let mut next = state.borrow().clone();
    next.status = status;
    let _ = state.send(next);
}

fn report_ok(state: &watch::Sender<MihomoState>, status: MihomoStatus, notice: impl Into<String>) {
    let _ = state.send(MihomoState {
        status,
        error: None,
        notice: Some(notice.into()),
    });
}

fn report_error(
    state: &watch::Sender<MihomoState>,
    status: Option<MihomoStatus>,
    message: impl Into<String>,
) {
    let mut next = state.borrow().clone();
    if let Some(status) = status {
        next.status = status;
    }
    next.error = Some(message.into());
    next.notice = None;
    let _ = state.send(next);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribe_waits_for_the_first_state_change() {
        let folder = tempfile::tempdir().unwrap();
        let service = MihomoService::new(folder.path().join("mihomo"));
        let mut state = service.subscribe();
        assert!(
            tokio::time::timeout(Duration::from_millis(150), state.changed())
                .await
                .is_err()
        );
    }

    #[test]
    fn invalid_start_configuration_reports_a_typed_error() {
        let folder = tempfile::tempdir().unwrap();
        let service = MihomoService::new(folder.path().join("mihomo"));
        service.start(MihomoConfig::default()).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state = service.state();
            if let Some(error) = state.error {
                assert!(error.contains("请选择 mihomo 可执行文件"), "{error}");
                assert_eq!(state.status, MihomoStatus::Stopped);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "mihomo service did not report the validation error"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
