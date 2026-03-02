use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gateway_core::{ConfigService, ProcessManager};
use tokio::sync::{broadcast, RwLock};

#[derive(Clone)]
pub struct AppState {
    pub config_service: ConfigService,
    pub process_manager: ProcessManager,
    pub started_at: DateTime<Utc>,
    pub sse_hub: SseHub,
}

#[derive(Clone)]
pub struct SseHub {
    channels: Arc<RwLock<HashMap<String, broadcast::Sender<String>>>>,
}

impl SseHub {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn subscribe(&self, server_id: &str) -> broadcast::Receiver<String> {
        let sender = self.get_or_create_sender(server_id).await;
        sender.subscribe()
    }

    pub async fn publish(&self, server_id: &str, payload: String) {
        let sender = self.get_or_create_sender(server_id).await;
        let _ = sender.send(payload);
    }

    async fn get_or_create_sender(&self, server_id: &str) -> broadcast::Sender<String> {
        {
            let guard = self.channels.read().await;
            if let Some(sender) = guard.get(server_id) {
                return sender.clone();
            }
        }

        let mut guard = self.channels.write().await;
        if let Some(sender) = guard.get(server_id) {
            return sender.clone();
        }

        let (sender, _receiver) = broadcast::channel(128);
        guard.insert(server_id.to_string(), sender.clone());
        sender
    }
}

impl Default for SseHub {
    fn default() -> Self {
        Self::new()
    }
}
