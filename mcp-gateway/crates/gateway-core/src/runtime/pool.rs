use std::time::Instant;

use tokio::sync::Mutex;

use super::connection::ProcessConnection;

pub struct PooledEntry {
    pub signature: String,
    pub connection: Mutex<ProcessConnection>,
    pub last_used: Mutex<Instant>,
}

impl PooledEntry {
    pub fn new(signature: String, connection: ProcessConnection) -> Self {
        Self {
            signature,
            connection: Mutex::new(connection),
            last_used: Mutex::new(Instant::now()),
        }
    }

    pub async fn touch(&self) {
        let mut guard = self.last_used.lock().await;
        *guard = Instant::now();
    }

    pub async fn shutdown(&self) {
        let mut conn = self.connection.lock().await;
        let _ = conn.shutdown().await;
    }
}
