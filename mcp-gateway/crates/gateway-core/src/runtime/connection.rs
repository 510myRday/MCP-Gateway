use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::config::ServerConfig;
use crate::error::AppError;

use super::io_codec::{read_message, write_message};
use super::protocol_negotiation::NegotiatedStdioProtocol;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "windows")]
fn configure_spawn_command(command: &mut Command) {
    // Avoid flashing a new terminal window when the gateway launches stdio MCP servers.
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_spawn_command(_command: &mut Command) {}

const MAX_CAPTURED_STDERR_LINES: usize = 80;

pub struct ProcessConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stdio_protocol: NegotiatedStdioProtocol,
    stderr_lines: Arc<Mutex<VecDeque<String>>>,
}

impl ProcessConnection {
    pub async fn spawn(
        server: &ServerConfig,
        stdio_protocol: NegotiatedStdioProtocol,
    ) -> Result<Self, AppError> {
        let resolved_command = resolve_command(server.command.trim());
        let mut command = Command::new(&resolved_command);
        command.args(&server.args);

        if !server.cwd.trim().is_empty() {
            command.current_dir(Path::new(&server.cwd));
        }

        command.envs(server.env.clone());
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        configure_spawn_command(&mut command);

        let mut child = command.spawn().map_err(|error| {
            AppError::Upstream(format!(
                "failed to spawn MCP stdio server '{}' for {}: {error}",
                resolved_command, server.name
            ))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::Internal("missing stdin for spawned process".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::Internal("missing stdout for spawned process".to_string()))?;
        let stderr_lines = Arc::new(Mutex::new(VecDeque::with_capacity(
            MAX_CAPTURED_STDERR_LINES,
        )));

        if let Some(stderr) = child.stderr.take() {
            let stderr_lines = stderr_lines.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let message = line.trim();
                    if message.is_empty() {
                        continue;
                    }
                    let mut guard = stderr_lines.lock().await;
                    if guard.len() == MAX_CAPTURED_STDERR_LINES {
                        let _ = guard.pop_front();
                    }
                    guard.push_back(message.to_string());
                }
            });
        }

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stdio_protocol,
            stderr_lines,
        })
    }

    pub async fn request(
        &mut self,
        request: &serde_json::Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<serde_json::Value, AppError> {
        let expected_id = request.get("id").cloned();
        write_message(&mut self.stdin, request, self.stdio_protocol).await?;

        if expected_id.is_none() {
            return Ok(json!({"ok": true}));
        }

        let mut iterations: u32 = 0;
        while iterations < max_response_wait_iterations {
            let message = timeout(
                timeout_duration,
                read_message(&mut self.stdout, self.stdio_protocol),
            )
            .await
            .map_err(|_| {
                AppError::Upstream("request timed out waiting for stdio response".to_string())
            })??;

            if message.get("id") == expected_id.as_ref() {
                return Ok(message);
            }

            iterations = iterations.saturating_add(1);
        }

        Err(AppError::Upstream(format!(
            "exceeded max response wait iterations ({max_response_wait_iterations}) waiting for stdio response"
        )))
    }

    pub async fn notify(&mut self, notification: &serde_json::Value) -> Result<(), AppError> {
        write_message(&mut self.stdin, notification, self.stdio_protocol).await
    }

    pub async fn stderr_snapshot(&self) -> String {
        let guard = self.stderr_lines.lock().await;
        guard.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    pub async fn shutdown(&mut self) -> Result<(), AppError> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }

        let _ = self.child.start_kill();

        let _ = timeout(Duration::from_secs(2), self.child.wait()).await;
        Ok(())
    }
}

fn resolve_command(command: &str) -> String {
    if cfg!(target_os = "windows") && command.eq_ignore_ascii_case("npx") {
        return "npx.cmd".to_string();
    }
    command.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_npx_on_windows() {
        let resolved = resolve_command("npx");
        if cfg!(target_os = "windows") {
            assert_eq!(resolved, "npx.cmd");
        } else {
            assert_eq!(resolved, "npx");
        }
    }
}
