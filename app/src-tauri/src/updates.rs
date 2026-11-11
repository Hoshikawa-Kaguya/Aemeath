//! 检查更新：Velopack 的 `UpdateManager`，检查和下载都在阻塞线程上进行，不经过控制面的 actor。
//!
//! 正式构建连 GitHub Releases；开发构建不连网，用环境变量 `AEMEATH_UPDATE_DIR` 指定的本地目录。
//! 没有打包时（开发运行）`UpdateManager` 建不起来，界面不显示更新操作。

use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::{info, warn};
use velopack::sources::UpdateSource;
use velopack::{UpdateCheck, UpdateInfo, UpdateManager};

use crate::controller::{CommandError, ControllerHandle};
use crate::recording::RecorderStateView;

const RELEASES_PAGE: &str = "https://github.com/Hoshikawa-Kaguya/Aemeath/releases";
const UPDATE_EVENT: &str = "update-changed";

/// 只看最新的正式 release，打包时用 `--delta none`
#[cfg(feature = "production-identity")]
fn update_source() -> Option<Box<dyn UpdateSource>> {
    const RELEASE_FEED: &str = "https://github.com/Hoshikawa-Kaguya/Aemeath/releases/latest/download";
    Some(Box::new(velopack::sources::HttpSource::new(RELEASE_FEED)))
}

/// 开发构建不连网：只在设置了环境变量时，从它指向的本地目录检查更新，用来测试升级流程。
#[cfg(not(feature = "production-identity"))]
fn update_source() -> Option<Box<dyn UpdateSource>> {
    const UPDATE_DIR_VARIABLE: &str = "AEMEATH_UPDATE_DIR";
    let directory = std::env::var_os(UPDATE_DIR_VARIABLE)?;
    Some(Box::new(velopack::sources::FileSource::new(directory)))
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum UpdateState {
    Unchecked,
    Checking,
    UpToDate,
    Available {
        version: String,
    },
    Downloading {
        version: String,
        percent: i16,
    },
    /// 已下载，等用户确认重启
    Ready {
        version: String,
    },
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateView {
    pub current_version: String,
    /// 打包运行才有更新功能
    pub supported: bool,
    pub portable: bool,
    #[serde(flatten)]
    pub state: UpdateState,
}

struct Shared {
    app: AppHandle,
    manager: Option<UpdateManager>,
    state: Mutex<UpdateState>,
    pending: Mutex<Option<UpdateInfo>>,
}

/// 更新服务（Tauri 托管状态）。
#[derive(Clone)]
pub struct UpdateService {
    shared: Arc<Shared>,
}

impl UpdateService {
    pub fn start(app: AppHandle) -> Self {
        let manager = update_source().and_then(|source| {
            UpdateManager::new_boxed(source, None, None)
                .inspect_err(|error| info!(%error, "not running from a package, updates are off"))
                .ok()
        });
        // 上次下载好但还没应用的更新
        let state = match manager.as_ref().and_then(UpdateManager::get_update_pending_restart) {
            Some(asset) => UpdateState::Ready { version: asset.Version },
            None => UpdateState::Unchecked,
        };
        Self {
            shared: Arc::new(Shared {
                app,
                manager,
                state: Mutex::new(state),
                pending: Mutex::new(None),
            }),
        }
    }

    pub fn view(&self) -> UpdateView {
        let manager = self.shared.manager.as_ref();
        UpdateView {
            current_version: manager.map_or_else(
                || aemeath_runtime::VERSION.to_owned(),
                UpdateManager::get_current_version_as_string,
            ),
            supported: manager.is_some(),
            portable: manager.is_some_and(UpdateManager::get_is_portable),
            state: self.state(),
        }
    }

    fn state(&self) -> UpdateState {
        self.shared.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set_state(&self, state: UpdateState) {
        *self.shared.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
        if let Err(error) = self.shared.app.emit(UPDATE_EVENT, self.view()) {
            warn!(%error, "cannot push the update state to the window");
        }
    }

    /// 检查更新。正在检查、下载或已下载好时不重复进行。
    pub fn check(&self) {
        let Some(manager) = self.shared.manager.clone() else {
            return;
        };
        if matches!(
            self.state(),
            UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready { .. }
        ) {
            return;
        }
        self.set_state(UpdateState::Checking);
        let service = self.clone();
        tauri::async_runtime::spawn_blocking(move || match manager.check_for_updates() {
            Ok(UpdateCheck::UpdateAvailable(update)) => {
                let version = update.TargetFullRelease.Version.clone();
                info!(%version, "an update is available");
                *service.shared.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(*update);
                service.set_state(UpdateState::Available { version });
            }
            Ok(UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty) => service.set_state(UpdateState::UpToDate),
            // 检查失败不打扰，只在设置页显示
            Err(error) => {
                warn!(%error, "the update check failed");
                service.set_state(UpdateState::Failed);
            }
        });
    }

    /// 下载检查到的更新，进度经事件推给界面。
    pub fn download(&self) {
        let Some(manager) = self.shared.manager.clone() else {
            return;
        };
        let pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(update) = pending else {
            return;
        };
        if matches!(self.state(), UpdateState::Downloading { .. }) {
            return;
        }
        let version = update.TargetFullRelease.Version.clone();
        self.set_state(UpdateState::Downloading {
            version: version.clone(),
            percent: 0,
        });
        let service = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let (progress, receiver) = channel();
            let reporter = service.clone();
            let report_version = version.clone();
            let forward = std::thread::spawn(move || {
                for percent in receiver {
                    reporter.set_state(UpdateState::Downloading {
                        version: report_version.clone(),
                        percent,
                    });
                }
            });
            let result = manager.download_updates(&update, Some(progress));
            let _ = forward.join();
            match result {
                Ok(()) => service.set_state(UpdateState::Ready { version }),
                Err(error) => {
                    warn!(%error, "the update download failed");
                    service.set_state(UpdateState::Failed);
                }
            }
        });
    }

    /// 应用已下载的更新：安排 Velopack 在本进程退出后替换文件并重启，然后整体退出。录音进行中时禁止。
    pub async fn apply(&self, controller: &ControllerHandle) -> Result<(), CommandError> {
        let failed = |message: String| CommandError::Failed { message };
        let manager = self
            .shared
            .manager
            .as_ref()
            .ok_or_else(|| failed("updates are not available".to_owned()))?;
        let state = controller.state().await?;
        let recording = state.nodes.iter().any(|node| {
            node.recorder
                .as_ref()
                .is_some_and(|recorder| matches!(recorder.state, RecorderStateView::Recording { .. }))
        });
        if recording {
            return Err(failed("a recording is in progress".to_owned()));
        }
        let asset = manager
            .get_update_pending_restart()
            .ok_or_else(|| failed("no update has been downloaded".to_owned()))?;
        // 先安排替换、成功后才停引擎：这一步失败时声音不受影响
        manager
            .wait_exit_then_apply_updates(asset, false, true, Vec::<String>::new())
            .map_err(|error| failed(error.to_string()))?;
        crate::tray::quit_app(&self.shared.app);
        Ok(())
    }

    /// 发布页：有新版本时是它的发布说明，否则是 Releases 列表。
    pub fn releases_url(&self) -> String {
        match self.state() {
            UpdateState::Available { version } => format!("{RELEASES_PAGE}/tag/v{version}"),
            _ => RELEASES_PAGE.to_owned(),
        }
    }
}
