use std::path::Path;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use crate::error::AppError;
use crate::terminal::wrap_windows_powershell_command_for_utf8;

use super::auth::{AuthOrchestrator, AuthSignalSource, PreparedServerLaunch, RuntimeAuthState};
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

pub struct ProcessConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stdio_protocol: NegotiatedStdioProtocol,
    auth_state: RuntimeAuthState,
}

impl ProcessConnection {
    pub async fn spawn(
        prepared: PreparedServerLaunch,
        stdio_protocol: NegotiatedStdioProtocol,
        auth: AuthOrchestrator,
    ) -> Result<Self, AppError> {
        let auth_state = RuntimeAuthState::new(auth.clone(), prepared.clone()).await;
        let resolved_command = resolve_command(prepared.server.command.trim());
        let (launch_command, launch_args) =
            wrap_windows_powershell_command_for_utf8(&resolved_command, &prepared.server.args)
                .unwrap_or_else(|| (resolved_command.clone(), prepared.server.args.clone()));
        let mut command = Command::new(&launch_command);
        command.args(&launch_args);

        if !prepared.server.cwd.trim().is_empty() {
            command.current_dir(Path::new(&prepared.server.cwd));
        }

        command.envs(prepared.server.env.clone());
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        configure_spawn_command(&mut command);

        let mut child = command.spawn().map_err(|error| {
            let message = format!(
                "failed to spawn MCP stdio server '{}' for {}: {error}",
                resolved_command, prepared.server.name
            );
            let error_message = message.clone();
            let auth = auth.clone();
            let prepared = prepared.clone();
            tokio::spawn(async move {
                auth.mark_launch_failed(&prepared, error_message).await;
            });
            AppError::Upstream(message)
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
            let auth_state = auth_state.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let message = line.trim();
                    if message.is_empty() {
                        continue;
                    }
                    auth_state
                        .handle_output_line(AuthSignalSource::Stderr, message.to_string())
                        .await;
                }
            });
        }

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stdio_protocol,
            auth_state,
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
        loop {
            let message = match timeout(
                timeout_duration,
                read_message(&mut self.stdout, self.stdio_protocol),
            )
            .await
            {
                Ok(Ok(message)) => message,
                Ok(Err(error)) => {
                    self.auth_state.mark_request_error(&error).await;
                    return Err(error);
                }
                Err(_) => {
                    if self.auth_state.should_continue_waiting_for_auth().await {
                        continue;
                    }
                    self.auth_state.mark_timeout().await;
                    return Err(AppError::Upstream(
                        "request timed out waiting for stdio response".to_string(),
                    ));
                }
            };

            if message.get("id") == expected_id.as_ref() {
                self.auth_state.mark_connected().await;
                return Ok(message);
            }

            iterations = iterations.saturating_add(1);
            if iterations >= max_response_wait_iterations {
                self.auth_state.mark_timeout().await;
                return Err(AppError::Upstream(format!(
                    "exceeded max response wait iterations ({max_response_wait_iterations}) waiting for stdio response"
                )));
            }
        }
    }

    pub async fn notify(&mut self, notification: &serde_json::Value) -> Result<(), AppError> {
        write_message(&mut self.stdin, notification, self.stdio_protocol).await
    }

    pub async fn stderr_snapshot(&self) -> String {
        self.auth_state.stderr_snapshot().await
    }

    pub async fn auth_state(&self) -> super::auth::ServerAuthState {
        self.auth_state.current_state().await
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
