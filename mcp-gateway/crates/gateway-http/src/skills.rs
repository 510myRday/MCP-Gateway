use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use gateway_core::{
    AppError, ErrorCode, GatewayConfig, SkillCommandRule, SkillPolicyAction, SkillsConfig,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{Notify, RwLock};
use utoipa::ToSchema;
use uuid::Uuid;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "windows")]
fn configure_skill_command(command: &mut Command) {
    // Keep skill scripts headless on Windows to avoid flashing cmd/powershell windows.
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_skill_command(_command: &mut Command) {}

#[derive(Clone, Default)]
pub struct SkillsService {
    confirmations: Arc<RwLock<HashMap<String, ConfirmationEntry>>>,
    discovery_cache: Arc<RwLock<Option<SkillDiscoveryCache>>>,
}

#[derive(Debug, Clone)]
struct ConfirmationEntry {
    /// 命令指纹：skill|command_preview，用于去重
    fingerprint: String,
    record: SkillConfirmation,
    notify: Arc<Notify>,
    timed_out: bool,
}

#[derive(Debug, Clone)]
struct SkillDiscoveryCache {
    signature: String,
    discovered: Vec<DiscoveredSkill>,
    expires_at: Instant,
}

#[derive(Debug, Clone, serde::Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillConfirmation {
    pub id: String,
    pub status: ConfirmationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub skill: String,
    pub script: String,
    pub args: Vec<String>,
    pub command_preview: String,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationStatus {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, serde::Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    pub skill: String,
    pub description: String,
    pub root: String,
    pub path: String,
    pub has_scripts: bool,
}

#[derive(Debug, Clone)]
struct DiscoveredSkill {
    skill: String,
    frontmatter_name: String,
    description: String,
    frontmatter_metadata: String,
    frontmatter_block: String,
    root: PathBuf,
    path: PathBuf,
    has_scripts: bool,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillCommandArgs {
    cmd: String,
}

#[derive(Debug)]
struct ToolResult {
    text: String,
    structured: Value,
    is_error: bool,
}

#[derive(Debug)]
enum PolicyDecision {
    Allow,
    Confirm(String),
    Deny(String),
}

#[derive(Debug, Clone)]
struct CommandInvocation {
    tokens: Vec<String>,
    raw: String,
    source: String,
}

#[derive(Debug)]
enum ConfirmationWaitOutcome {
    Approved,
    Rejected,
    TimedOut,
}

/// `create_confirmation` 的三种结果：
/// - `Created(record)`  — 新建了一条 Pending 确认，需要等用户决定
/// - `Reused(record)`   — 同指纹已有 Pending 条目，复用它，继续等待
/// - `AlreadyTimedOut(id)` — 同指纹的上一次请求刚超时，直接拒绝，不再弹窗
#[derive(Debug)]
enum CreateConfirmationResult {
    Created(SkillConfirmation),
    Reused(SkillConfirmation),
    AlreadyTimedOut(String),
}

#[derive(Debug, Clone, Default)]
struct ParsedFrontmatter {
    name: String,
    description: String,
    metadata: String,
    block: String,
}

#[derive(Debug)]
struct StreamCapturedOutput {
    text: String,
    truncated: bool,
}

#[derive(Debug)]
struct SkillCommandExecution {
    status: std::process::ExitStatus,
    stdout: StreamCapturedOutput,
    stderr: StreamCapturedOutput,
}

impl SkillsService {
    const CONFIRMATION_DECISION_TIMEOUT: Duration = Duration::from_secs(60);
    const CONFIRMATION_STALE_PENDING_WINDOW: Duration = Duration::from_secs(75);
    const CONFIRMATION_RESOLVED_RETENTION_WINDOW: Duration = Duration::from_secs(120);
    const SKILL_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(3);

    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_skills_server(&self, config: &GatewayConfig, server_name: &str) -> bool {
        config.skills.enabled && config.skills.server_name == server_name
    }

    pub async fn handle_mcp_request(&self, config: &GatewayConfig, request: Value) -> Value {
        let Some(object) = request.as_object() else {
            return jsonrpc_error(Value::Null, -32600, "invalid request payload", None);
        };

        let id = object.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return jsonrpc_error(id, -32600, "missing jsonrpc method", None);
        };

        if !config.skills.enabled {
            return jsonrpc_error(id, -32001, "skills server is disabled", None);
        }

        match method {
            "initialize" => jsonrpc_result(
                id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    },
                    "serverInfo": {
                        "name": "mcp-gateway-skills",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ),
            "ping" | "notifications/initialized" => jsonrpc_result(id, json!({"ok": true})),
            "tools/list" => {
                let discovered = match self.discover_skills(&config.skills).await {
                    Ok(skills) => skills,
                    Err(error) => {
                        return jsonrpc_error(
                            id,
                            -32603,
                            "failed to discover skills",
                            Some(json!({"detail": error.to_string()})),
                        );
                    }
                };
                let summaries = summarize_discovered_skills(&discovered);
                jsonrpc_result(
                    id,
                    json!({
                        "tools": tool_definitions(&discovered),
                        "skills": summaries
                    }),
                )
            }
            "tools/call" => {
                let params = object.get("params").cloned().unwrap_or(Value::Null);
                let tool_params: ToolCallParams = match serde_json::from_value(params) {
                    Ok(value) => value,
                    Err(error) => {
                        return jsonrpc_error(
                            id,
                            -32602,
                            "invalid tool call params",
                            Some(json!({"detail": error.to_string()})),
                        );
                    }
                };

                let result = match self.execute_tool_call(config, tool_params).await {
                    Ok(output) => output,
                    Err(error) => error_to_tool_result(error),
                };
                jsonrpc_result(
                    id,
                    json!({
                        "isError": result.is_error,
                        "content": [
                            {
                                "type": "text",
                                "text": result.text
                            }
                        ],
                        "structuredContent": result.structured
                    }),
                )
            }
            _ => jsonrpc_error(id, -32601, "method not found", None),
        }
    }

    pub async fn list_pending_confirmations(&self) -> Vec<SkillConfirmation> {
        let now = Utc::now();
        let mut guard = self.confirmations.write().await;
        Self::prune_confirmations_locked(&mut guard, now);
        let mut list = guard
            .values()
            .filter(|entry| entry.record.status == ConfirmationStatus::Pending)
            .map(|entry| entry.record.clone())
            .collect::<Vec<_>>();
        list.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        list
    }

    pub async fn approve_confirmation(&self, id: &str) -> Result<SkillConfirmation, AppError> {
        let now = Utc::now();
        let mut guard = self.confirmations.write().await;
        Self::prune_confirmations_locked(&mut guard, now);
        let Some(entry) = guard.get_mut(id) else {
            return Err(AppError::NotFound("confirmation not found".to_string()));
        };
        let notify = entry.notify.clone();
        match entry.record.status {
            ConfirmationStatus::Pending => {}
            ConfirmationStatus::Approved => {
                return Err(AppError::Conflict(
                    "confirmation already approved".to_string(),
                ));
            }
            ConfirmationStatus::Rejected => {
                return Err(AppError::Conflict(
                    "confirmation already rejected".to_string(),
                ));
            }
        }
        entry.record.status = ConfirmationStatus::Approved;
        entry.record.updated_at = now;
        entry.timed_out = false;
        let updated = entry.record.clone();
        notify.notify_one();
        Ok(updated)
    }

    pub async fn reject_confirmation(&self, id: &str) -> Result<SkillConfirmation, AppError> {
        let now = Utc::now();
        let mut guard = self.confirmations.write().await;
        Self::prune_confirmations_locked(&mut guard, now);
        let Some(target) = guard.get(id).map(|entry| entry.record.clone()) else {
            return Err(AppError::NotFound("confirmation not found".to_string()));
        };
        match target.status {
            ConfirmationStatus::Pending => {}
            ConfirmationStatus::Approved => {
                return Err(AppError::Conflict(
                    "confirmation already approved".to_string(),
                ));
            }
            ConfirmationStatus::Rejected => {
                return Err(AppError::Conflict(
                    "confirmation already rejected".to_string(),
                ));
            }
        }
        let mut notifies = Vec::new();
        for entry in guard.values_mut() {
            if entry.record.status != ConfirmationStatus::Pending {
                continue;
            }
            if Self::is_same_confirmation_signature(&entry.record, &target) {
                entry.record.status = ConfirmationStatus::Rejected;
                entry.record.updated_at = now;
                entry.timed_out = false;
                notifies.push(entry.notify.clone());
            }
        }
        for notify in notifies {
            notify.notify_one();
        }

        guard
            .get(id)
            .map(|entry| entry.record.clone())
            .ok_or_else(|| AppError::NotFound("confirmation not found".to_string()))
    }

    pub async fn list_skills_for_admin(
        &self,
        config: &GatewayConfig,
    ) -> Result<Vec<SkillSummary>, AppError> {
        self.discover_skills(&config.skills)
            .await
            .map(|skills| summarize_discovered_skills(&skills))
    }

    async fn execute_tool_call(
        &self,
        config: &GatewayConfig,
        params: ToolCallParams,
    ) -> Result<ToolResult, AppError> {
        let skills = self.discover_skills(&config.skills).await?;
        let bindings = build_skill_tool_bindings(&skills);
        let Some((tool_name, skill)) = bindings
            .iter()
            .find(|(tool_name, _)| tool_name.eq_ignore_ascii_case(params.name.as_str()))
            .map(|(tool_name, skill)| (tool_name.clone(), (*skill).clone()))
        else {
            return Err(AppError::BadRequest(format!(
                "unknown tool name: {}",
                params.name
            )));
        };

        let args = decode_tool_args::<SkillCommandArgs>(&params.arguments)?;
        self.handle_skill_command(config, &tool_name, &skill, args)
            .await
    }

    async fn handle_skill_command(
        &self,
        config: &GatewayConfig,
        tool_name: &str,
        skill: &DiscoveredSkill,
        args: SkillCommandArgs,
    ) -> Result<ToolResult, AppError> {
        let command_preview = args.cmd.trim().to_string();
        if command_preview.is_empty() {
            return Err(AppError::BadRequest("cmd cannot be empty".to_string()));
        }

        let tokens = split_shell_tokens(&command_preview);
        if tokens.is_empty() {
            return Err(AppError::BadRequest("cmd cannot be empty".to_string()));
        }
        let program = tokens[0].clone();
        let command_args = tokens[1..].to_vec();

        let skill_md_path = skill.path.join("SKILL.md");
        let policy = evaluate_policy(
            &config.skills,
            &program,
            &command_args,
            &command_preview,
            &skill_md_path,
            None,
        );
        match policy {
            PolicyDecision::Deny(reason) => {
                let reason_text = reason.clone();
                return Ok(tool_error(
                    format!("command blocked by policy: {reason_text}"),
                    json!({
                        "status": "blocked",
                        "reason": reason,
                        "command": command_preview
                    }),
                ));
            }
            PolicyDecision::Confirm(reason) => {
                let (confirmation_id, already_decided) = match self
                    .create_confirmation(
                        &skill.skill,
                        &command_preview,
                        &tokens,
                        &command_preview,
                        &reason,
                    )
                    .await
                {
                    // 全新确认 → 正常走等待流程
                    CreateConfirmationResult::Created(c) => (c.id, None),
                    // 同指纹已有 Pending → 复用同一个 id，继续等待
                    CreateConfirmationResult::Reused(c) => (c.id, None),
                    // 同指纹刚超时 → 直接拒绝，不再弹窗
                    CreateConfirmationResult::AlreadyTimedOut(id) => {
                        (id.clone(), Some(ConfirmationWaitOutcome::TimedOut))
                    }
                };

                let outcome = match already_decided {
                    Some(decided) => decided,
                    None => {
                        self.wait_for_confirmation_decision(
                            &confirmation_id,
                            Self::CONFIRMATION_DECISION_TIMEOUT,
                            Duration::from_millis(250),
                        )
                        .await
                    }
                };

                match outcome {
                    ConfirmationWaitOutcome::Approved => {}
                    ConfirmationWaitOutcome::Rejected => {
                        return Ok(confirmation_rejected_result(&confirmation_id, false));
                    }
                    ConfirmationWaitOutcome::TimedOut => {
                        return Ok(confirmation_rejected_result(&confirmation_id, true));
                    }
                }
            }
            PolicyDecision::Allow => {}
        }

        let timeout_ms = config.skills.execution.timeout_ms.max(1000);
        let max_output_bytes = config.skills.execution.max_output_bytes.max(1024);
        let (runner, runner_args) = shell_command_for_current_os(&command_preview);

        let started = Instant::now();
        let mut command = Command::new(&runner);
        command
            .args(&runner_args)
            .current_dir(&skill.path)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        configure_skill_command(&mut command);

        let disable_truncation = should_disable_output_truncation(&program, &command_args);
        let output = execute_skill_command(
            &mut command,
            timeout_ms,
            max_output_bytes,
            disable_truncation,
        )
        .await?;
        let duration_ms = started.elapsed().as_millis() as u64;
        let stdout = output.stdout.text;
        let stderr = output.stderr.text;
        let stdout_truncated = output.stdout.truncated;
        let stderr_truncated = output.stderr.truncated;
        let exit_code = output.status.code().unwrap_or(-1);

        let mut structured = serde_json::Map::new();
        structured.insert(
            "status".to_string(),
            Value::String(if output.status.success() {
                "completed".to_string()
            } else {
                "failed".to_string()
            }),
        );
        structured.insert("tool".to_string(), Value::String(tool_name.to_string()));
        structured.insert("skill".to_string(), Value::String(skill.skill.clone()));
        structured.insert("command".to_string(), Value::String(command_preview));
        structured.insert("exitCode".to_string(), json!(exit_code));
        structured.insert("durationMs".to_string(), json!(duration_ms));
        if stdout_truncated {
            structured.insert("stdoutTruncated".to_string(), Value::Bool(true));
        }
        if stderr_truncated {
            structured.insert("stderrTruncated".to_string(), Value::Bool(true));
        }
        let structured = Value::Object(structured);

        let output_text = command_output_text(&stdout, &stderr);

        if output.status.success() {
            Ok(tool_success(output_text, structured))
        } else {
            Ok(tool_error(
                command_failure_text(exit_code, &stdout, &stderr),
                structured,
            ))
        }
    }

    async fn create_confirmation(
        &self,
        skill: &str,
        script: &str,
        args: &[String],
        command_preview: &str,
        reason: &str,
    ) -> CreateConfirmationResult {
        let fingerprint = format!("{skill}|{command_preview}");
        let now = Utc::now();
        let mut guard = self.confirmations.write().await;
        Self::prune_confirmations_locked(&mut guard, now);

        // 检查同指纹是否已有条目：
        // - Pending  → 复用，不重复弹窗
        // - 刚超时的 Rejected (timed_out=true) → 直接告知调用方已超时，不新建
        // - 用户手动 Rejected / Approved → 允许重新发起
        for entry in guard.values() {
            if entry.fingerprint != fingerprint {
                continue;
            }
            match entry.record.status {
                ConfirmationStatus::Pending => {
                    return CreateConfirmationResult::Reused(entry.record.clone());
                }
                ConfirmationStatus::Rejected if entry.timed_out => {
                    return CreateConfirmationResult::AlreadyTimedOut(entry.record.id.clone());
                }
                _ => {}
            }
        }

        let id = Uuid::new_v4().to_string();
        let record = SkillConfirmation {
            id: id.clone(),
            status: ConfirmationStatus::Pending,
            created_at: now,
            updated_at: now,
            skill: skill.to_string(),
            script: script.to_string(),
            args: args.to_vec(),
            command_preview: command_preview.to_string(),
            reason: reason.to_string(),
        };

        guard.insert(
            id,
            ConfirmationEntry {
                fingerprint,
                record: record.clone(),
                notify: Arc::new(Notify::new()),
                timed_out: false,
            },
        );
        let timeout_service = self.clone();
        let timeout_id = record.id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Self::CONFIRMATION_DECISION_TIMEOUT).await;
            timeout_service
                .reject_confirmation_on_timeout(&timeout_id)
                .await;
        });
        CreateConfirmationResult::Created(record)
    }

    async fn wait_for_confirmation_decision(
        &self,
        confirmation_id: &str,
        timeout: Duration,
        _poll_interval: Duration,
    ) -> ConfirmationWaitOutcome {
        let started = Instant::now();
        loop {
            let wait_notify = {
                let now = Utc::now();
                let mut guard = self.confirmations.write().await;
                Self::prune_confirmations_locked(&mut guard, now);
                match guard.get(confirmation_id).map(|entry| {
                    (
                        entry.record.status.clone(),
                        entry.record.created_at,
                        entry.notify.clone(),
                        entry.timed_out,
                    )
                }) {
                    Some((ConfirmationStatus::Approved, _, _, _)) => {
                        guard.remove(confirmation_id);
                        return ConfirmationWaitOutcome::Approved;
                    }
                    Some((ConfirmationStatus::Rejected, _, _, timed_out)) => {
                        guard.remove(confirmation_id);
                        return if timed_out {
                            ConfirmationWaitOutcome::TimedOut
                        } else {
                            ConfirmationWaitOutcome::Rejected
                        };
                    }
                    Some((ConfirmationStatus::Pending, created_at, notify, _)) => {
                        if Self::age_exceeds(created_at, now, timeout) {
                            if let Some(entry) = guard.get_mut(confirmation_id) {
                                entry.record.status = ConfirmationStatus::Rejected;
                                entry.record.updated_at = now;
                                entry.timed_out = true;
                                entry.notify.notify_one();
                            }
                            return ConfirmationWaitOutcome::TimedOut;
                        }
                        notify
                    }
                    None => return ConfirmationWaitOutcome::TimedOut,
                }
            };

            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                let mut guard = self.confirmations.write().await;
                if let Some(entry) = guard.get_mut(confirmation_id) {
                    entry.record.status = ConfirmationStatus::Rejected;
                    entry.record.updated_at = Utc::now();
                    entry.timed_out = true;
                    entry.notify.notify_one();
                }
                return ConfirmationWaitOutcome::TimedOut;
            };
            if remaining.is_zero() {
                let mut guard = self.confirmations.write().await;
                if let Some(entry) = guard.get_mut(confirmation_id) {
                    entry.record.status = ConfirmationStatus::Rejected;
                    entry.record.updated_at = Utc::now();
                    entry.timed_out = true;
                    entry.notify.notify_one();
                }
                return ConfirmationWaitOutcome::TimedOut;
            }

            let notified = tokio::time::timeout(remaining, wait_notify.notified()).await;
            if notified.is_err() {
                let mut guard = self.confirmations.write().await;
                if let Some(entry) = guard.get_mut(confirmation_id) {
                    entry.record.status = ConfirmationStatus::Rejected;
                    entry.record.updated_at = Utc::now();
                    entry.timed_out = true;
                    entry.notify.notify_one();
                }
                return ConfirmationWaitOutcome::TimedOut;
            }
        }
    }

    async fn reject_confirmation_on_timeout(&self, id: &str) {
        let now = Utc::now();
        let mut guard = self.confirmations.write().await;
        Self::prune_confirmations_locked(&mut guard, now);
        let Some(entry) = guard.get_mut(id) else {
            return;
        };
        if entry.record.status != ConfirmationStatus::Pending {
            return;
        }
        entry.record.status = ConfirmationStatus::Rejected;
        entry.record.updated_at = now;
        entry.timed_out = true;
        entry.notify.notify_one();
    }

    fn age_exceeds(created_at: DateTime<Utc>, now: DateTime<Utc>, ttl: Duration) -> bool {
        now.signed_duration_since(created_at)
            .to_std()
            .map(|elapsed| elapsed >= ttl)
            .unwrap_or(false)
    }

    fn is_same_confirmation_signature(left: &SkillConfirmation, right: &SkillConfirmation) -> bool {
        left.skill == right.skill
            && left.script == right.script
            && left.args == right.args
            && left.command_preview == right.command_preview
            && left.reason == right.reason
    }

    fn prune_confirmations_locked(
        confirmations: &mut HashMap<String, ConfirmationEntry>,
        now: DateTime<Utc>,
    ) {
        confirmations.retain(|_, entry| match entry.record.status {
            ConfirmationStatus::Pending => !Self::age_exceeds(
                entry.record.created_at,
                now,
                Self::CONFIRMATION_STALE_PENDING_WINDOW,
            ),
            ConfirmationStatus::Approved | ConfirmationStatus::Rejected => !Self::age_exceeds(
                entry.record.updated_at,
                now,
                Self::CONFIRMATION_RESOLVED_RETENTION_WINDOW,
            ),
        });
    }

    async fn discover_skills(
        &self,
        skills_config: &SkillsConfig,
    ) -> Result<Vec<DiscoveredSkill>, AppError> {
        let roots = skills_config.roots.clone();
        let signature = roots_signature(&roots);

        {
            let now = Instant::now();
            let guard = self.discovery_cache.read().await;
            if let Some(cached) = guard.as_ref() {
                if cached.signature == signature && now <= cached.expires_at {
                    return Ok(cached.discovered.clone());
                }
            }
        }

        let roots_for_scan = roots.clone();
        let discovered = tokio::task::spawn_blocking(move || discover_skills_sync(&roots_for_scan))
            .await
            .map_err(|error| AppError::Internal(format!("skills discovery join error: {error}")))??;

        {
            let mut guard = self.discovery_cache.write().await;
            *guard = Some(SkillDiscoveryCache {
                signature,
                discovered: discovered.clone(),
                expires_at: Instant::now() + Self::SKILL_DISCOVERY_CACHE_TTL,
            });
        }

        Ok(discovered)
    }
}

fn decode_tool_args<T>(value: &Value) -> Result<T, AppError>
where
    T: for<'de> Deserialize<'de>,
{
    let payload = if value.is_null() {
        json!({})
    } else {
        value.clone()
    };
    serde_json::from_value(payload)
        .map_err(|error| AppError::BadRequest(format!("invalid tool arguments: {error}")))
}

fn tool_success(text: String, structured: Value) -> ToolResult {
    ToolResult {
        text,
        structured,
        is_error: false,
    }
}

fn tool_error(text: String, structured: Value) -> ToolResult {
    ToolResult {
        text,
        structured,
        is_error: true,
    }
}

fn confirmation_rejected_result(confirmation_id: &str, timed_out: bool) -> ToolResult {
    let text = if timed_out {
        "confirmation timed out after 60 seconds; auto rejected"
    } else {
        "user rejected confirmation request"
    };
    let reason = if timed_out {
        "timeout"
    } else {
        "user_rejected"
    };
    tool_success(
        text.to_string(),
        json!({
            "status": "rejected",
            "reason": reason,
            "confirmationId": confirmation_id
        }),
    )
}

fn error_to_tool_result(error: AppError) -> ToolResult {
    let code = format!("{:?}", error.code());
    let message = error.message();
    let structured = json!({
        "status": "error",
        "code": code,
        "message": message.clone()
    });
    ToolResult {
        text: message,
        structured,
        is_error: matches!(
            error.code(),
            ErrorCode::BadRequest
                | ErrorCode::ValidationFailed
                | ErrorCode::NotFound
                | ErrorCode::Conflict
                | ErrorCode::UpstreamFailed
                | ErrorCode::Internal
                | ErrorCode::Unauthorized
        ),
    }
}

fn command_output_text(stdout: &str, stderr: &str) -> String {
    let stdout = stdout.trim_end();
    let stderr = stderr.trim_end();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n\n[stderr]\n{stderr}"),
        (true, true) => "command completed with no output".to_string(),
    }
}

fn command_failure_text(exit_code: i32, stdout: &str, stderr: &str) -> String {
    let output = command_output_text(stdout, stderr);
    if output == "command completed with no output" {
        format!("command finished with non-zero exit code ({exit_code}) and no output")
    } else {
        format!("command finished with non-zero exit code ({exit_code}).\n{output}")
    }
}

fn summarize_discovered_skills(skills: &[DiscoveredSkill]) -> Vec<SkillSummary> {
    skills
        .iter()
        .map(|skill| SkillSummary {
            skill: skill.skill.clone(),
            description: skill.description.clone(),
            root: normalize_display_path(&skill.root),
            path: normalize_display_path(&skill.path),
            has_scripts: skill.has_scripts,
        })
        .collect()
}

fn tool_definitions(skills: &[DiscoveredSkill]) -> Value {
    let bindings = build_skill_tool_bindings(skills);
    let now = Utc::now().to_rfc3339();
    let os = current_os_label();

    Value::Array(
        bindings
            .into_iter()
            .map(|(tool_name, skill)| {
                let description = render_skill_tool_description(skill, os, &now);
                json!({
                    "name": tool_name,
                    "description": description,
                    "inputSchema": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["cmd"],
                        "properties": {
                            "cmd": {
                                "type": "string",
                                "description": "Shell command string for this skill. Main uses: read markdown files with full paths (for example `cat D:/.../SKILL.md` or `Get-Content D:/.../SKILL.md`) and run scripts."
                            }
                        }
                    }
                })
            })
            .collect(),
    )
}

fn build_skill_tool_bindings(skills: &[DiscoveredSkill]) -> Vec<(String, &DiscoveredSkill)> {
    let mut sorted = skills.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|skill| skill.skill.to_ascii_lowercase());

    let mut used = HashMap::<String, usize>::new();
    let mut bindings = Vec::with_capacity(sorted.len());
    for skill in sorted {
        let base = skill_tool_name_base(skill);
        let next = used
            .entry(base.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let tool_name = if *next == 1 {
            base
        } else {
            format!("{}_{}", base, *next)
        };
        bindings.push((tool_name, skill));
    }
    bindings
}

fn skill_tool_name_base(skill: &DiscoveredSkill) -> String {
    let raw = if !skill.frontmatter_name.trim().is_empty() {
        skill.frontmatter_name.trim()
    } else {
        skill.skill.trim()
    };
    sanitize_tool_name(raw)
}

fn sanitize_tool_name(raw: &str) -> String {
    let mut out = String::new();
    let mut last_separator = false;

    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_separator = false;
            continue;
        }
        if matches!(ch, '-' | '_') {
            out.push(ch);
            last_separator = false;
            continue;
        }
        if !last_separator {
            out.push('_');
            last_separator = true;
        }
    }

    let trimmed = out.trim_matches('_').trim_matches('-').to_string();
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed
    }
}

fn render_skill_tool_description(skill: &DiscoveredSkill, os: &str, now: &str) -> String {
    let meta_description = if skill.description.trim().is_empty() {
        format!("Skill instructions for {}", skill.skill)
    } else {
        skill.description.trim().to_string()
    };
    let frontmatter_block = if skill.frontmatter_block.trim().is_empty() {
        "none".to_string()
    } else {
        format!("---\n{}\n---", skill.frontmatter_block.trim())
    };
    let skill_path = normalize_display_path(&skill.path);
    format!(
        "To learn the complete usage of this skill, run `cmd` to read the full SKILL.md text. like `cat /.../SKILL.md` or `Get-Content D:/.../SKILL.md`. The `cmd` value should be one shell command string used either to read markdown files or run scripts.\nCurrent OS: {os}.\nCurrent datetime: {now}.\nSkill path: {skill_path}.\nFront matter summary:\nname: {}\ndescription: {}\nmetadata: {}\nFront matter raw (YAML):\n{}",
        if skill.frontmatter_name.trim().is_empty() {
            skill.skill.trim()
        } else {
            skill.frontmatter_name.trim()
        },
        meta_description,
        if skill.frontmatter_metadata.trim().is_empty() {
            "none"
        } else {
            skill.frontmatter_metadata.trim()
        },
        frontmatter_block
    )
}

fn current_os_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "Unknown"
    }
}

fn normalize_display_path(path: &Path) -> String {
    let raw = path.to_string_lossy().to_string();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = raw.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    raw
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn jsonrpc_error(id: Value, code: i32, message: &str, data: Option<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": data
        }
    })
}

fn discover_skills_sync(roots: &[String]) -> Result<Vec<DiscoveredSkill>, AppError> {
    let mut discovered = Vec::new();
    let mut seen_skill_dirs = HashSet::new();

    for root in roots {
        let root_path = PathBuf::from(root);
        if !root_path.is_dir() {
            continue;
        }

        let mut stack = vec![root_path.clone()];
        let mut seen_dirs = HashSet::new();

        while let Some(current_dir) = stack.pop() {
            let canonical_dir =
                std::fs::canonicalize(&current_dir).unwrap_or_else(|_| current_dir.clone());
            if !seen_dirs.insert(canonical_dir.clone()) {
                continue;
            }

            register_skill_directory(
                &root_path,
                &canonical_dir,
                &mut seen_skill_dirs,
                &mut discovered,
            );

            let entries = match std::fs::read_dir(&canonical_dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for entry in entries {
                let Ok(entry) = entry else {
                    continue;
                };
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_dir() || file_type.is_symlink() {
                    continue;
                }
                stack.push(entry.path());
            }
        }
    }

    discovered.sort_by(|left, right| left.skill.to_lowercase().cmp(&right.skill.to_lowercase()));
    Ok(discovered)
}

fn register_skill_directory(
    root_path: &Path,
    dir_path: &Path,
    seen_skill_dirs: &mut HashSet<PathBuf>,
    discovered: &mut Vec<DiscoveredSkill>,
) {
    let skill_md = dir_path.join("SKILL.md");
    if !skill_md.is_file() {
        return;
    }

    let canonical_skill_dir = std::fs::canonicalize(dir_path).unwrap_or_else(|_| dir_path.into());
    if !seen_skill_dirs.insert(canonical_skill_dir.clone()) {
        return;
    }

    let dir_name = canonical_skill_dir
        .file_name()
        .and_then(OsStr::to_str)
        .map(str::to_string)
        .unwrap_or_else(|| canonical_skill_dir.to_string_lossy().to_string());
    let parsed_frontmatter = parse_frontmatter_fields(&skill_md).unwrap_or_default();

    discovered.push(DiscoveredSkill {
        skill: dir_name.clone(),
        frontmatter_name: parsed_frontmatter.name,
        description: parsed_frontmatter.description,
        frontmatter_metadata: parsed_frontmatter.metadata,
        frontmatter_block: parsed_frontmatter.block,
        root: root_path.to_path_buf(),
        has_scripts: canonical_skill_dir.join("scripts").is_dir(),
        path: canonical_skill_dir,
    });
}

fn parse_frontmatter_fields(skill_md_path: &Path) -> Result<ParsedFrontmatter, AppError> {
    let content = std::fs::read_to_string(skill_md_path)?;
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Ok(ParsedFrontmatter::default());
    }

    let mut frontmatter_lines = Vec::new();
    let mut has_closing = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            has_closing = true;
            break;
        }
        frontmatter_lines.push(line.to_string());
    }
    if !has_closing {
        return Ok(ParsedFrontmatter::default());
    }

    let raw = frontmatter_lines.join("\n").trim().to_string();
    if raw.trim().is_empty() {
        return Ok(ParsedFrontmatter::default());
    }

    let frontmatter: Value = serde_yaml::from_str(&raw).map_err(|error| {
        AppError::BadRequest(format!(
            "invalid YAML frontmatter in {}: {error}",
            skill_md_path.display()
        ))
    })?;
    let frontmatter_obj = frontmatter.as_object().ok_or_else(|| {
        AppError::BadRequest(format!(
            "frontmatter in {} must be a YAML mapping",
            skill_md_path.display()
        ))
    })?;

    let name = frontmatter_obj
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let description = frontmatter_obj
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let metadata = frontmatter_obj
        .get("metadata")
        .map(frontmatter_value_summary)
        .unwrap_or_else(|| "none".to_string());
    Ok(ParsedFrontmatter {
        name,
        description,
        metadata,
        block: raw,
    })
}

fn frontmatter_value_summary(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::String(text) => text.to_string(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "unserializable".to_string()),
    }
}

fn shell_command_for_current_os(cmd: &str) -> (String, Vec<String>) {
    if cfg!(target_os = "windows") {
        (
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-Command".to_string(),
                cmd.to_string(),
            ],
        )
    } else {
        ("sh".to_string(), vec!["-lc".to_string(), cmd.to_string()])
    }
}

fn roots_signature(roots: &[String]) -> String {
    let mut normalized = roots
        .iter()
        .map(|root| root.trim())
        .filter(|root| !root.is_empty())
        .map(|root| normalize_lexical_path(Path::new(root)).to_string_lossy().to_string())
        .collect::<Vec<_>>();
    normalized.sort_by_key(|item| item.to_ascii_lowercase());
    normalized.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    normalized.join("\u{001f}")
}

async fn execute_skill_command(
    command: &mut Command,
    timeout_ms: u64,
    max_output_bytes: usize,
    disable_truncation: bool,
) -> Result<SkillCommandExecution, AppError> {
    let mut child = command
        .spawn()
        .map_err(|error| AppError::Upstream(format!("failed to execute command: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Internal("missing stdout from skill command".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Internal("missing stderr from skill command".to_string()))?;

    let stdout_task = tokio::spawn(capture_stream_output(
        stdout,
        max_output_bytes,
        disable_truncation,
    ));
    let stderr_task = tokio::spawn(capture_stream_output(
        stderr,
        max_output_bytes,
        disable_truncation,
    ));

    let status = match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(wait_result) => {
            wait_result.map_err(|error| AppError::Upstream(format!("failed to execute command: {error}")))?
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(AppError::Upstream(format!(
                "command timed out after {timeout_ms}ms"
            )));
        }
    };

    let stdout = stdout_task
        .await
        .map_err(|error| AppError::Internal(format!("stdout capture join error: {error}")))??;
    let stderr = stderr_task
        .await
        .map_err(|error| AppError::Internal(format!("stderr capture join error: {error}")))??;

    Ok(SkillCommandExecution {
        status,
        stdout,
        stderr,
    })
}

async fn capture_stream_output<R>(
    mut reader: R,
    max_output_bytes: usize,
    disable_truncation: bool,
) -> Result<StreamCapturedOutput, AppError>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut chunk = [0_u8; 8192];
    let mut captured = if disable_truncation {
        Vec::with_capacity(8192)
    } else {
        Vec::with_capacity(max_output_bytes.min(8192))
    };
    let mut truncated = false;

    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|error| AppError::Upstream(format!("failed to read command output: {error}")))?;
        if read == 0 {
            break;
        }

        if disable_truncation {
            captured.extend_from_slice(&chunk[..read]);
            continue;
        }

        if captured.len() < max_output_bytes {
            let available = max_output_bytes - captured.len();
            let take = available.min(read);
            captured.extend_from_slice(&chunk[..take]);
            if take < read {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }

    Ok(StreamCapturedOutput {
        text: String::from_utf8_lossy(&captured).to_string(),
        truncated,
    })
}

fn evaluate_policy(
    skills: &SkillsConfig,
    program: &str,
    command_args: &[String],
    raw_command: &str,
    script_path: &Path,
    script_text: Option<&str>,
) -> PolicyDecision {
    let invocations = collect_command_invocations(program, command_args, raw_command, script_text);
    for invocation in &invocations {
        for rule in &skills.policy.rules {
            if rule_matches(rule, invocation) {
                let base_reason = if rule.reason.trim().is_empty() {
                    "matched command rule".to_string()
                } else {
                    rule.reason.trim().to_string()
                };
                let reason = format!(
                    "{} (rule: {}, source: {})",
                    base_reason, rule.id, invocation.source
                );
                return action_to_decision(&rule.action, reason);
            }
        }
    }

    if skills.policy.path_guard.enabled && !skills.policy.path_guard.whitelist_dirs.is_empty() {
        let whitelist = skills
            .policy
            .path_guard
            .whitelist_dirs
            .iter()
            .map(PathBuf::from)
            .map(normalize_root_path)
            .collect::<Vec<_>>();

        if let Some((token, source, resolved)) =
            find_outside_whitelist_path(script_path, command_args, script_text, &whitelist)
        {
            let reason = format!(
                "path '{}' resolved to '{}' is outside whitelist (source: {})",
                token,
                resolved.to_string_lossy(),
                source
            );
            return action_to_decision(&skills.policy.path_guard.on_violation, reason);
        }
    }

    action_to_decision(
        &skills.policy.default_action,
        "matched default policy".to_string(),
    )
}

fn action_to_decision(action: &SkillPolicyAction, reason: String) -> PolicyDecision {
    match action {
        SkillPolicyAction::Allow => PolicyDecision::Allow,
        SkillPolicyAction::Confirm => PolicyDecision::Confirm(reason),
        SkillPolicyAction::Deny => PolicyDecision::Deny(reason),
    }
}

fn collect_command_invocations(
    program: &str,
    command_args: &[String],
    raw_command: &str,
    script_text: Option<&str>,
) -> Vec<CommandInvocation> {
    let mut list = Vec::new();
    let mut runtime_tokens = Vec::with_capacity(command_args.len() + 1);
    runtime_tokens.push(normalize_command_token(program));
    runtime_tokens.extend(command_args.iter().map(|item| item.to_ascii_lowercase()));
    list.push(CommandInvocation {
        raw: std::iter::once(program.to_string())
            .chain(command_args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" "),
        tokens: runtime_tokens,
        source: "runtime".to_string(),
    });

    for (index, segment) in split_command_segments(raw_command).into_iter().enumerate() {
        let mut tokens = split_shell_tokens(&segment);
        if tokens.is_empty() {
            continue;
        }
        if tokens[0] == "&" {
            tokens.remove(0);
        }
        if tokens.is_empty() {
            continue;
        }

        let mut normalized = Vec::with_capacity(tokens.len());
        normalized.push(normalize_command_token(&tokens[0]));
        normalized.extend(tokens.iter().skip(1).map(|item| item.to_ascii_lowercase()));

        list.push(CommandInvocation {
            raw: segment,
            tokens: normalized,
            source: format!("runtime:segment:{}", index + 1),
        });
    }

    if let Some(script) = script_text {
        for (line_no, line) in script.lines().enumerate().take(300) {
            let trimmed = line.trim();
            if trimmed.is_empty() || is_comment_line(trimmed) {
                continue;
            }

            let mut tokens = split_shell_tokens(trimmed);
            if tokens.is_empty() {
                continue;
            }
            if tokens[0] == "&" {
                tokens.remove(0);
            }
            if tokens.is_empty() {
                continue;
            }

            let mut normalized = Vec::with_capacity(tokens.len());
            normalized.push(normalize_command_token(&tokens[0]));
            normalized.extend(tokens.iter().skip(1).map(|item| item.to_ascii_lowercase()));

            list.push(CommandInvocation {
                raw: trimmed.to_string(),
                tokens: normalized,
                source: format!("script:{}", line_no + 1),
            });
        }
    }

    list
}

fn split_command_segments(raw: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            current.push(ch);
            continue;
        }

        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                current.push(ch);
            }
            ';' => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    segments.push(trimmed.to_string());
                }
                current.clear();
            }
            '|' => {
                if chars.peek().copied() == Some('|') {
                    let _ = chars.next();
                }
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    segments.push(trimmed.to_string());
                }
                current.clear();
            }
            '&' => {
                if chars.peek().copied() == Some('&') {
                    let _ = chars.next();
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        segments.push(trimmed.to_string());
                    }
                    current.clear();
                } else {
                    current.push(ch);
                }
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }

    segments
}

fn rule_matches(rule: &SkillCommandRule, invocation: &CommandInvocation) -> bool {
    if !rule.command_tree.is_empty() {
        if invocation.tokens.len() < rule.command_tree.len() {
            return false;
        }
        for (idx, node) in rule.command_tree.iter().enumerate() {
            if invocation.tokens[idx] != *node {
                return false;
            }
        }
    }

    if !rule.contains.is_empty() {
        let raw = invocation.raw.to_ascii_lowercase();
        for needle in &rule.contains {
            let matched_in_tokens = invocation.tokens.iter().any(|token| token.contains(needle));
            if !matched_in_tokens && !raw.contains(needle) {
                return false;
            }
        }
    }

    true
}

fn find_outside_whitelist_path(
    script_path: &Path,
    command_args: &[String],
    script_text: Option<&str>,
    whitelist: &[PathBuf],
) -> Option<(String, String, PathBuf)> {
    let script_file = normalize_root_path(script_path.to_path_buf());
    let script_dir = script_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    for (token, source) in collect_path_candidates(command_args, script_text) {
        let resolved = resolve_candidate_path(&script_dir, &token);
        if resolved == script_file {
            continue;
        }
        let allowed = whitelist.iter().any(|root| resolved.starts_with(root));
        if !allowed {
            return Some((token, source, resolved));
        }
    }
    None
}

fn collect_path_candidates(
    command_args: &[String],
    script_text: Option<&str>,
) -> Vec<(String, String)> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for (index, arg) in command_args.iter().enumerate() {
        for token in split_shell_tokens(arg) {
            let cleaned = strip_matching_quotes(&token);
            if is_path_like(cleaned) && seen.insert(cleaned.to_string()) {
                candidates.push((cleaned.to_string(), format!("arg:{index}")));
            }
        }
    }

    if let Some(script) = script_text {
        for (line_no, line) in script.lines().enumerate().take(300) {
            let trimmed = line.trim();
            if trimmed.is_empty() || is_comment_line(trimmed) {
                continue;
            }
            let tokens = split_shell_tokens(trimmed);
            for token in tokens.into_iter().skip(1) {
                let cleaned = strip_matching_quotes(&token);
                if is_path_like(cleaned) && seen.insert(cleaned.to_string()) {
                    candidates.push((cleaned.to_string(), format!("script:{}", line_no + 1)));
                }
            }
        }
    }

    candidates
}

fn resolve_candidate_path(script_dir: &Path, token: &str) -> PathBuf {
    let expanded = expand_home_path(token);
    let raw = PathBuf::from(expanded);
    let absolute = if raw.is_absolute() {
        raw
    } else {
        script_dir.join(raw)
    };
    normalize_root_path(absolute)
}

fn normalize_root_path(path: PathBuf) -> PathBuf {
    if path.exists() {
        match std::fs::canonicalize(&path) {
            Ok(value) => value,
            Err(_) => normalize_lexical_path(&path),
        }
    } else {
        normalize_lexical_path(&path)
    }
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn expand_home_path(token: &str) -> String {
    if let Some(rest) = token.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    if token == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().to_string();
        }
    }
    token.to_string()
}

fn normalize_command_token(token: &str) -> String {
    let cleaned = strip_matching_quotes(token);
    Path::new(cleaned)
        .file_name()
        .and_then(|item| item.to_str())
        .unwrap_or(cleaned)
        .to_ascii_lowercase()
}

fn is_comment_line(line: &str) -> bool {
    line.starts_with('#')
        || line.starts_with("//")
        || line.starts_with("::")
        || line
            .get(0..3)
            .map(|prefix| prefix.eq_ignore_ascii_case("rem"))
            .unwrap_or(false)
}

fn split_shell_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    for ch in line.chars() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            ' ' | '\t' => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn strip_matching_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn is_path_like(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if token.starts_with('-') {
        return false;
    }
    if token.contains("://") {
        return false;
    }
    if token.starts_with("~/")
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with('\\')
        || token.starts_with('/')
        || token.contains('\\')
        || token.contains('/')
    {
        return true;
    }
    let bytes = token.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn should_disable_output_truncation(program: &str, command_args: &[String]) -> bool {
    let normalized_program = normalize_command_token(program);
    let is_markdown_reader = matches!(
        normalized_program.as_str(),
        "cat" | "type" | "get-content" | "gc"
    );
    if !is_markdown_reader {
        return false;
    }

    command_args.iter().any(|arg| {
        let candidate = strip_matching_quotes(arg).trim();
        if candidate.is_empty() || candidate.starts_with('-') {
            return false;
        }

        let file_name = Path::new(candidate)
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or(candidate);
        let lowered = file_name.to_ascii_lowercase();
        lowered == "skill.md" || lowered.ends_with(".md") || lowered.ends_with(".markdown")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn command_tree_rule_can_trigger_confirmation() {
        let skills = SkillsConfig::default();
        let raw = "sh script.sh";
        let decision = evaluate_policy(
            &skills,
            "sh",
            &[String::from("script.sh")],
            raw,
            Path::new("scripts/script.sh"),
            Some("rm test.txt"),
        );

        match decision {
            PolicyDecision::Confirm(reason) => {
                assert!(reason.contains("confirm-rm"));
            }
            _ => panic!("expected confirm decision"),
        }
    }

    #[test]
    fn path_guard_blocks_outside_whitelist() {
        let mut skills = SkillsConfig::default();
        let base = std::env::current_dir().expect("cwd");
        let allowed = base.join("allowed-zone");
        let blocked = base.join("outside-zone").join("target.txt");

        skills.policy.path_guard.enabled = true;
        skills.policy.path_guard.whitelist_dirs = vec![allowed.to_string_lossy().to_string()];
        skills.policy.path_guard.on_violation = SkillPolicyAction::Deny;
        skills.policy.rules.clear();

        let raw = format!("python tool.py {}", blocked.to_string_lossy());
        let decision = evaluate_policy(
            &skills,
            "python",
            &[
                String::from("tool.py"),
                blocked.to_string_lossy().to_string(),
            ],
            &raw,
            &allowed.join("scripts").join("tool.py"),
            None,
        );

        match decision {
            PolicyDecision::Deny(reason) => {
                assert!(reason.contains("outside whitelist"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    #[test]
    fn default_policy_allow_when_no_match() {
        let mut skills = SkillsConfig::default();
        skills.policy.rules.clear();
        skills.policy.path_guard.enabled = false;
        skills.policy.default_action = SkillPolicyAction::Allow;

        let raw = "python safe.py --help";
        let decision = evaluate_policy(
            &skills,
            "python",
            &[String::from("safe.py"), String::from("--help")],
            raw,
            Path::new("scripts/safe.py"),
            Some("print('ok')"),
        );

        match decision {
            PolicyDecision::Allow => {}
            _ => panic!("expected allow decision"),
        }
    }

    #[test]
    fn shell_wrapper_chain_is_blocked_by_default_rules() {
        let skills = SkillsConfig::default();
        let raw = "bash -lc \"rm -rf /tmp/demo\"";
        let decision = evaluate_policy(
            &skills,
            "bash",
            &[String::from("-lc"), String::from("rm -rf /tmp/demo")],
            raw,
            Path::new("scripts/safe.sh"),
            None,
        );

        match decision {
            PolicyDecision::Deny(reason) => {
                assert!(reason.contains("deny-bash-lc"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    #[tokio::test]
    async fn confirmation_wait_returns_approved_when_user_approves() {
        let service = SkillsService::new();
        let confirmation = match service
            .create_confirmation(
                "skill",
                "cmd",
                &[String::from("cmd")],
                "cmd",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created, got {other:?}"),
        };
        let confirmation_id = confirmation.id.clone();
        let service_for_approve = service.clone();
        let id_for_approve = confirmation_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = service_for_approve
                .approve_confirmation(&id_for_approve)
                .await;
        });

        let outcome = service
            .wait_for_confirmation_decision(
                &confirmation_id,
                Duration::from_secs(2),
                Duration::from_millis(10),
            )
            .await;
        match outcome {
            ConfirmationWaitOutcome::Approved => {}
            _ => panic!("expected approved outcome"),
        }
    }

    #[tokio::test]
    async fn create_confirmation_creates_distinct_requests_for_same_command() {
        let service = SkillsService::new();
        let first = match service
            .create_confirmation(
                "skill",
                "cmd-repeat",
                &[String::from("cmd-repeat")],
                "cmd-repeat",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created, got {other:?}"),
        };
        // 同指纹第二次调用 → Reused，复用同一条记录
        let second = match service
            .create_confirmation(
                "skill",
                "cmd-repeat",
                &[String::from("cmd-repeat")],
                "cmd-repeat",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Reused(c) => c,
            other => panic!("expected Reused, got {other:?}"),
        };
        assert_eq!(first.id, second.id);
        assert_eq!(first.status, ConfirmationStatus::Pending);
        assert_eq!(second.status, ConfirmationStatus::Pending);
    }

    #[tokio::test]
    async fn confirmation_wait_returns_rejected_when_user_rejects() {
        let service = SkillsService::new();
        let confirmation = match service
            .create_confirmation(
                "skill",
                "cmd-reject-wait",
                &[String::from("cmd-reject-wait")],
                "cmd-reject-wait",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created, got {other:?}"),
        };
        let confirmation_id = confirmation.id.clone();
        let service_for_reject = service.clone();
        let id_for_reject = confirmation_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = service_for_reject.reject_confirmation(&id_for_reject).await;
        });

        let outcome = service
            .wait_for_confirmation_decision(
                &confirmation_id,
                Duration::from_secs(2),
                Duration::from_millis(10),
            )
            .await;
        match outcome {
            ConfirmationWaitOutcome::Rejected => {}
            _ => panic!("expected rejected outcome"),
        }
    }

    #[tokio::test]
    async fn rejecting_one_confirmation_rejects_pending_duplicates() {
        let service = SkillsService::new();
        // 第一次：新建
        let first = match service
            .create_confirmation(
                "skill",
                "cmd-dup",
                &[String::from("cmd-dup")],
                "cmd-dup",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created for first, got {other:?}"),
        };
        // 第二次同指纹：复用
        let second = match service
            .create_confirmation(
                "skill",
                "cmd-dup",
                &[String::from("cmd-dup")],
                "cmd-dup",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Reused(c) => c,
            other => panic!("expected Reused for second, got {other:?}"),
        };
        assert_eq!(
            first.id, second.id,
            "same fingerprint should reuse the same entry"
        );
        let _ = service
            .reject_confirmation(&first.id)
            .await
            .expect("reject first");

        let second_outcome = service
            .wait_for_confirmation_decision(
                &second.id,
                Duration::from_secs(1),
                Duration::from_millis(10),
            )
            .await;
        match second_outcome {
            ConfirmationWaitOutcome::Rejected => {}
            _ => panic!("expected duplicate request to be rejected"),
        }
    }

    #[tokio::test]
    async fn concurrent_create_confirmation_requests_are_distinct() {
        let service = SkillsService::new();
        let s1 = service.clone();
        let s2 = service.clone();

        let one = tokio::spawn(async move {
            s1.create_confirmation(
                "skill",
                "cmd-dedupe",
                &[String::from("cmd-dedupe")],
                "cmd-dedupe",
                "need approval",
            )
            .await
        });
        let two = tokio::spawn(async move {
            s2.create_confirmation(
                "skill",
                "cmd-dedupe",
                &[String::from("cmd-dedupe")],
                "cmd-dedupe",
                "need approval",
            )
            .await
        });

        let first = one.await.expect("first join");
        let second = two.await.expect("second join");
        // 并发同指纹：一个 Created，另一个 Reused，二者复用同一条记录
        let first_id = match &first {
            CreateConfirmationResult::Created(c) | CreateConfirmationResult::Reused(c) => {
                c.id.clone()
            }
            CreateConfirmationResult::AlreadyTimedOut(id) => id.clone(),
        };
        let second_id = match &second {
            CreateConfirmationResult::Created(c) | CreateConfirmationResult::Reused(c) => {
                c.id.clone()
            }
            CreateConfirmationResult::AlreadyTimedOut(id) => id.clone(),
        };
        assert_eq!(
            first_id, second_id,
            "concurrent same-fingerprint calls should share one entry"
        );
    }

    #[tokio::test]
    async fn confirmation_wait_times_out_when_not_confirmed() {
        let service = SkillsService::new();
        let confirmation = match service
            .create_confirmation(
                "skill",
                "cmd-timeout",
                &[String::from("cmd-timeout")],
                "cmd-timeout",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created, got {other:?}"),
        };

        let outcome = service
            .wait_for_confirmation_decision(
                &confirmation.id,
                Duration::from_millis(40),
                Duration::from_millis(10),
            )
            .await;
        match outcome {
            ConfirmationWaitOutcome::TimedOut => {}
            _ => panic!("expected timed out outcome"),
        }
        let pending = service.list_pending_confirmations().await;
        assert!(pending.iter().all(|item| item.id != confirmation.id));
    }

    #[tokio::test]
    async fn rejected_confirmation_then_same_command_still_creates_new_request() {
        let service = SkillsService::new();
        let first = match service
            .create_confirmation(
                "skill",
                "cmd-reject",
                &[String::from("cmd-reject")],
                "cmd-reject",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created, got {other:?}"),
        };
        // 用户手动拒绝（timed_out=false）→ 允许重新发起
        let _ = service.reject_confirmation(&first.id).await;

        let reused = match service
            .create_confirmation(
                "skill",
                "cmd-reject",
                &[String::from("cmd-reject")],
                "cmd-reject",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created (new entry after user reject), got {other:?}"),
        };
        assert_ne!(reused.id, first.id);
        assert_eq!(reused.status, ConfirmationStatus::Pending);

        let outcome = service
            .wait_for_confirmation_decision(
                &first.id,
                Duration::from_secs(1),
                Duration::from_millis(10),
            )
            .await;
        match outcome {
            ConfirmationWaitOutcome::Rejected => {}
            _ => panic!("expected rejected outcome"),
        }
    }

    #[tokio::test]
    async fn approved_confirmation_then_same_command_still_creates_new_request() {
        let service = SkillsService::new();
        let first = match service
            .create_confirmation(
                "skill",
                "cmd-approve",
                &[String::from("cmd-approve")],
                "cmd-approve",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created, got {other:?}"),
        };
        let _ = service.approve_confirmation(&first.id).await;

        let reused = match service
            .create_confirmation(
                "skill",
                "cmd-approve",
                &[String::from("cmd-approve")],
                "cmd-approve",
                "need approval",
            )
            .await
        {
            CreateConfirmationResult::Created(c) => c,
            other => panic!("expected Created (new entry after approve), got {other:?}"),
        };
        assert_ne!(reused.id, first.id);
        assert_eq!(reused.status, ConfirmationStatus::Pending);

        let outcome = service
            .wait_for_confirmation_decision(
                &first.id,
                Duration::from_secs(1),
                Duration::from_millis(10),
            )
            .await;
        match outcome {
            ConfirmationWaitOutcome::Approved => {}
            _ => panic!("expected approved outcome"),
        }
    }

    #[test]
    fn chained_remove_item_requires_confirmation() {
        let skills = SkillsConfig::default();
        let raw = "Set-Location 'D:/Code_Save/demo'; Remove-Item -Force package-lock.json -ErrorAction SilentlyContinue; npm install";
        let tokens = split_shell_tokens(raw);
        let decision = evaluate_policy(
            &skills,
            &tokens[0],
            &tokens[1..],
            raw,
            Path::new("scripts/safe.ps1"),
            None,
        );

        match decision {
            PolicyDecision::Confirm(reason) => {
                assert!(reason.contains("confirm-remove-item"));
            }
            _ => panic!("expected confirm decision"),
        }
    }

    #[test]
    fn markdown_read_commands_disable_output_truncation() {
        assert!(should_disable_output_truncation(
            "Get-Content",
            &[
                String::from("-Raw"),
                String::from("D:/skills/demo/SKILL.md")
            ]
        ));
        assert!(should_disable_output_truncation(
            "cat",
            &[String::from("./references/weather_info.md")]
        ));
        assert!(!should_disable_output_truncation(
            "python",
            &[String::from("scripts/tool.py")]
        ));
    }

    #[test]
    fn command_output_text_formats_stdout_and_stderr() {
        assert_eq!(command_output_text("hello\n", ""), "hello");
        assert_eq!(command_output_text("", "warn\n"), "warn");
        assert_eq!(
            command_output_text("ok\n", "warn\n"),
            "ok\n\n[stderr]\nwarn"
        );
    }

    #[test]
    fn command_failure_text_includes_exit_code() {
        assert_eq!(
            command_failure_text(2, "", ""),
            "command finished with non-zero exit code (2) and no output"
        );
        assert_eq!(
            command_failure_text(2, "oops\n", ""),
            "command finished with non-zero exit code (2).\noops"
        );
    }

    #[test]
    fn discover_skills_sync_finds_root_and_nested_skill_directories() {
        let sandbox = std::env::temp_dir().join(format!("gateway-skills-{}", Uuid::new_v4()));
        let root_skill_dir = sandbox.join("root-skill");
        let nested_skill_dir = root_skill_dir.join("nested").join("child-skill");
        std::fs::create_dir_all(&nested_skill_dir).expect("create test directories");
        std::fs::write(
            root_skill_dir.join("SKILL.md"),
            "---\nname: root-skill\n---\n# Root\n",
        )
        .expect("write root SKILL.md");
        std::fs::write(
            nested_skill_dir.join("SKILL.md"),
            "---\nname: child-skill\n---\n# Child\n",
        )
        .expect("write nested SKILL.md");

        let roots = vec![root_skill_dir.to_string_lossy().to_string()];
        let discovered = discover_skills_sync(&roots).expect("discover skills");
        let names = discovered
            .iter()
            .map(|skill| skill.skill.clone())
            .collect::<HashSet<_>>();

        assert!(names.contains("root-skill"));
        assert!(names.contains("child-skill"));

        let _ = std::fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn parse_frontmatter_fields_reads_yaml_mapping_and_metadata() {
        let sandbox = std::env::temp_dir().join(format!("gateway-frontmatter-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&sandbox).expect("create sandbox");
        let skill_md = sandbox.join("SKILL.md");
        std::fs::write(
            &skill_md,
            "---\nname: demo-skill\ndescription: Demo description\nmetadata:\n  tags:\n    - remotion\n    - video\n  image:\n    format: png\n---\n# Demo\n",
        )
        .expect("write SKILL.md");

        let parsed = parse_frontmatter_fields(&skill_md).expect("parse frontmatter");
        assert_eq!(parsed.name, "demo-skill");
        assert_eq!(parsed.description, "Demo description");
        assert!(parsed
            .metadata
            .contains("\"tags\":[\"remotion\",\"video\"]"));
        assert!(parsed.metadata.contains("\"image\":{\"format\":\"png\"}"));
        assert!(parsed.block.contains("metadata:"));

        let _ = std::fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn tool_definitions_use_metadata_name_and_cmd_schema() {
        let discovered = vec![
            DiscoveredSkill {
                skill: "alpha".to_string(),
                frontmatter_name: "Alpha Skill".to_string(),
                description: "A".to_string(),
                frontmatter_metadata: r#"{"tags":["alpha"]}"#.to_string(),
                frontmatter_block:
                    "name: Alpha Skill\ndescription: A\nmetadata:\n  tags:\n    - alpha".to_string(),
                root: PathBuf::from("C:/skills"),
                path: PathBuf::from("C:/skills/alpha"),
                has_scripts: true,
            },
            DiscoveredSkill {
                skill: "beta".to_string(),
                frontmatter_name: "Beta Skill".to_string(),
                description: "B".to_string(),
                frontmatter_metadata: "none".to_string(),
                frontmatter_block: "name: Beta Skill\ndescription: B".to_string(),
                root: PathBuf::from("C:/skills"),
                path: PathBuf::from("C:/skills/beta"),
                has_scripts: false,
            },
        ];
        let tools = tool_definitions(&discovered);
        let alpha_tool = tools
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("name").and_then(Value::as_str) == Some("alpha_skill"))
            })
            .expect("alpha skill tool exists");
        let cmd_schema = alpha_tool
            .get("inputSchema")
            .and_then(|schema| schema.get("properties"))
            .and_then(|props| props.get("cmd"))
            .expect("cmd schema is present");
        assert_eq!(
            cmd_schema.get("type").and_then(Value::as_str),
            Some("string")
        );

        let description = alpha_tool
            .get("description")
            .and_then(Value::as_str)
            .expect("tool description");
        assert!(description.contains("Current OS:"));
        assert!(description.contains("Current datetime:"));
        assert!(description.contains("SKILL.md"));
        assert!(description.contains("Front matter summary:"));
        assert!(description.contains("name: Alpha Skill"));
        assert!(description.contains("metadata: {\"tags\":[\"alpha\"]}"));
        assert!(description.contains("Front matter raw (YAML):"));
        assert!(description.contains("metadata:"));

        let names = tools
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|item| item.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha_skill", "beta_skill"]);
    }
}
