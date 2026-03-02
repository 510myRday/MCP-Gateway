use std::path::Path;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use crate::config::{ServerConfig, StdioProtocol};
use crate::error::AppError;

use super::io_codec::{read_message, write_message};

pub struct ProcessConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stdio_protocol: StdioProtocol,
}

impl ProcessConnection {
    pub async fn spawn(server: &ServerConfig) -> Result<Self, AppError> {
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

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let message = line.trim();
                    if message.is_empty() {
                        continue;
                    }
                }
            });
        }

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stdio_protocol: server.stdio_protocol.clone(),
        })
    }

    pub async fn request(
        &mut self,
        request: &serde_json::Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<serde_json::Value, AppError> {
        let expected_id = request.get("id").cloned();
        write_message(&mut self.stdin, request, &self.stdio_protocol).await?;

        if expected_id.is_none() {
            return Ok(json!({"ok": true}));
        }

        let mut iterations: u32 = 0;
        while iterations < max_response_wait_iterations {
            let message = timeout(
                timeout_duration,
                read_message(&mut self.stdout, &self.stdio_protocol),
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
        write_message(&mut self.stdin, notification, &self.stdio_protocol).await
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
