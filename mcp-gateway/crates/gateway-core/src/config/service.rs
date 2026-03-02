use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};

use crate::error::AppError;

use super::model::{
    load_config_from_path, normalize_config_in_place, save_config_atomic, GatewayConfig,
};
use super::validate::validate_config;

#[derive(Debug, Clone)]
struct VersionedConfig {
    config: GatewayConfig,
}

#[derive(Clone)]
pub struct ConfigService {
    path: Arc<PathBuf>,
    state: Arc<RwLock<VersionedConfig>>,
    update_lock: Arc<Mutex<()>>,
}

impl ConfigService {
    pub async fn from_path(path: PathBuf) -> Result<Self, AppError> {
        let cfg = load_config_async(path.clone()).await?;
        Ok(Self {
            path: Arc::new(path),
            state: Arc::new(RwLock::new(VersionedConfig { config: cfg })),
            update_lock: Arc::new(Mutex::new(())),
        })
    }

    pub async fn get_config(&self) -> GatewayConfig {
        self.state.read().await.config.clone()
    }

    pub async fn replace(&self, mut next: GatewayConfig) -> Result<GatewayConfig, AppError> {
        normalize_config_in_place(&mut next);
        validate_config(&next)?;
        self.update(|_| Ok(next)).await
    }

    pub async fn update<F>(&self, f: F) -> Result<GatewayConfig, AppError>
    where
        F: FnOnce(&GatewayConfig) -> Result<GatewayConfig, AppError> + Send,
    {
        let _guard = self.update_lock.lock().await;
        let current = self.state.read().await.config.clone();
        let mut next = f(&current)?;
        normalize_config_in_place(&mut next);
        validate_config(&next)?;
        save_config_async(self.path.as_ref().clone(), next.clone()).await?;

        let mut state_guard = self.state.write().await;
        state_guard.config = next.clone();
        Ok(next)
    }
}

async fn load_config_async(path: PathBuf) -> Result<GatewayConfig, AppError> {
    tokio::task::spawn_blocking(move || load_config_from_path(path.as_path()))
        .await
        .map_err(|err| AppError::Internal(format!("join error when loading config: {err}")))?
}

async fn save_config_async(path: PathBuf, cfg: GatewayConfig) -> Result<(), AppError> {
    tokio::task::spawn_blocking(move || save_config_atomic(path.as_path(), &cfg))
        .await
        .map_err(|err| AppError::Internal(format!("join error when saving config: {err}")))?
}
