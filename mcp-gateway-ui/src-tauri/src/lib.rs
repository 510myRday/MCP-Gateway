use std::collections::HashSet;
use std::net::TcpListener;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;
use std::{fs, io};

use chrono::Utc;
use gateway_core::{
    load_config_from_path, ConfigService, GatewayConfig, ProcessManager, ServerConfig,
    SkillCommandRule,
};
use gateway_http::{build_router, spawn_idle_reaper, AppState, SkillsService, SseHub};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Manager, State};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const EMBEDDED_RUNTIME_ID: &str = "embedded://gateway-runtime";

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "windows")]
fn configure_ui_command(command: &mut Command) {
    // Avoid flashing a console window when running helper shell commands.
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_ui_command(_command: &mut Command) {}

struct ManagedGateway {
    task: JoinHandle<()>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    config_path: PathBuf,
}

#[derive(Default)]
struct GatewayProcessState {
    inner: Mutex<Option<ManagedGateway>>,
}

impl Drop for GatewayProcessState {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(mut managed) = guard.take() {
                if let Some(tx) = managed.shutdown_tx.take() {
                    let _ = tx.send(());
                }
                managed.task.abort();
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayProcessStatus {
    running: bool,
    pid: Option<u32>,
    launched_by_ui: bool,
    executable: Option<String>,
    config_path: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillDirectoryValidation {
    exists: bool,
    is_dir: bool,
    has_skill_md: bool,
}

fn stopped_status(last_error: Option<String>) -> GatewayProcessStatus {
    GatewayProcessStatus {
        running: false,
        pid: None,
        launched_by_ui: false,
        executable: None,
        config_path: None,
        last_error,
    }
}

fn running_status(config_path: &PathBuf) -> GatewayProcessStatus {
    GatewayProcessStatus {
        running: true,
        pid: Some(std::process::id()),
        launched_by_ui: true,
        executable: Some(EMBEDDED_RUNTIME_ID.to_string()),
        config_path: Some(config_path.display().to_string()),
        last_error: None,
    }
}

fn ensure_default_config_exists(path: &PathBuf) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }

    let default_config = default_local_config();
    let content = serde_json::to_string_pretty(&default_config)
        .map_err(|error| format!("序列化默认配置失败：{error}"))?;
    fs::write(path, content).map_err(io_error)?;
    Ok(())
}

fn ensure_listen_port_available(listen: &str) -> Result<(), String> {
    let Some(port) = parse_port_from_listen(listen) else {
        return Ok(());
    };

    if TcpListener::bind(("127.0.0.1", port)).is_ok() {
        return Ok(());
    }

    kill_port_process(port).map_err(|e| {
        format!("端口 {port} 被占用且无法自动清理：{e}\n请手动检查并关闭占用该端口的进程后重试。")
    })?;

    // 等待端口释放（最多 2 秒）
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(250));
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
    }

    Err(format!(
        "端口 {port} 仍被占用（进程已尝试终止但端口未释放）。请稍候再试或重启系统。"
    ))
}

async fn start_embedded_gateway(config_path: PathBuf) -> Result<ManagedGateway, String> {
    let config_path_for_load = config_path.clone();
    let cfg = tokio::task::spawn_blocking(move || load_config_from_path(&config_path_for_load))
        .await
        .map_err(|error| format!("加载配置任务失败：{error}"))?
        .map_err(|error| format!("加载配置失败：{error}"))?;

    let listen_for_check = cfg.listen.clone();
    tokio::task::spawn_blocking(move || ensure_listen_port_available(&listen_for_check))
        .await
        .map_err(|error| format!("端口检测任务失败：{error}"))??;

    let config_service = ConfigService::from_path(config_path.clone())
        .await
        .map_err(|error| format!("初始化配置服务失败：{error}"))?;

    let state = AppState {
        config_service,
        process_manager: ProcessManager::new(),
        started_at: Utc::now(),
        sse_hub: SseHub::new(),
        skills: SkillsService::new(),
    };

    let router = build_router(state.clone(), &cfg);
    spawn_idle_reaper(state.clone());

    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .map_err(|error| format!("启动网关失败：{error}"))?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let serve_result = axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
        if let Err(error) = serve_result {
            eprintln!("gateway serve failed: {error}");
        }
        state.process_manager.reset_pool().await;
    });

    Ok(ManagedGateway {
        task,
        shutdown_tx: Some(shutdown_tx),
        config_path,
    })
}

#[tauri::command]
fn gateway_status(state: State<GatewayProcessState>) -> GatewayProcessStatus {
    let mut guard = state.inner.lock().expect("gateway state poisoned");

    if let Some(managed) = guard.as_mut() {
        if managed.task.is_finished() {
            *guard = None;
            return stopped_status(None);
        }
        return running_status(&managed.config_path);
    }

    stopped_status(None)
}

#[tauri::command]
async fn start_gateway(
    state: State<'_, GatewayProcessState>,
) -> Result<GatewayProcessStatus, String> {
    {
        let mut guard = state.inner.lock().map_err(|_| "gateway state poisoned")?;
        if let Some(managed) = guard.as_mut() {
            if managed.task.is_finished() {
                *guard = None;
            } else {
                return Ok(running_status(&managed.config_path));
            }
        }
    }

    let config_path = resolve_default_config_path()?;
    ensure_default_config_exists(&config_path)?;
    let managed = start_embedded_gateway(config_path.clone()).await?;

    let mut guard = state.inner.lock().map_err(|_| "gateway state poisoned")?;
    *guard = Some(managed);
    Ok(running_status(&config_path))
}

#[tauri::command]
async fn stop_gateway(
    state: State<'_, GatewayProcessState>,
) -> Result<GatewayProcessStatus, String> {
    let managed = {
        let mut guard = state.inner.lock().map_err(|_| "gateway state poisoned")?;
        guard.take()
    };

    if let Some(mut managed) = managed {
        if let Some(tx) = managed.shutdown_tx.take() {
            let _ = tx.send(());
        }

        let wait_result = tokio::time::timeout(Duration::from_secs(3), &mut managed.task).await;
        if wait_result.is_err() {
            managed.task.abort();
        }
    }

    Ok(stopped_status(None))
}

// ── 解析地址字符串中的端口号 ──────────────────────────────────────
fn parse_port_from_listen(listen: &str) -> Option<u16> {
    // 支持 "127.0.0.1:8765" 或 "0.0.0.0:8765" 或 ":8765"
    if let Some(colon_pos) = listen.rfind(':') {
        listen[colon_pos + 1..].parse::<u16>().ok()
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn kill_port_process(port: u16) -> Result<String, String> {
    // netstat -ano 找 PID，再 taskkill
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", &format!(
            "for /f \"tokens=5\" %a in ('netstat -ano ^| findstr :{port} ^| findstr LISTENING') do taskkill /F /PID %a"
        )]);
    configure_ui_command(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| format!("执行端口清理命令失败: {e}"))?;

    if output.status.success() {
        Ok(format!("已清理端口 {port} 的占用进程"))
    } else {
        // 也尝试 PowerShell 方式
        let mut powershell = Command::new("powershell");
        powershell.args(["-NoProfile", "-Command", &format!(
                "Get-NetTCPConnection -LocalPort {port} -State Listen -ErrorAction SilentlyContinue | ForEach-Object {{ Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }}"
            )]);
        configure_ui_command(&mut powershell);
        let ps_output = powershell
            .output()
            .map_err(|e| format!("PowerShell 端口清理失败: {e}"))?;
        if ps_output.status.success() {
            Ok(format!("已通过 PowerShell 清理端口 {port}"))
        } else {
            Err(format!("无法清理端口 {port} 的占用进程"))
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn kill_port_process(port: u16) -> Result<String, String> {
    // lsof -ti :PORT | xargs kill -9
    let output = Command::new("sh")
        .args([
            "-c",
            &format!("lsof -ti :{port} | xargs kill -9 2>/dev/null; true"),
        ])
        .output()
        .map_err(|e| format!("执行端口清理命令失败: {e}"))?;

    if output.status.success() {
        Ok(format!("已清理端口 {port} 的占用进程"))
    } else {
        Err(format!("无法清理端口 {port} 的占用进程"))
    }
}

fn resolve_default_config_path() -> Result<PathBuf, String> {
    let mut base = dirs::config_dir().ok_or_else(|| "无法定位系统配置目录".to_string())?;
    base.push("mcp-gateway");
    base.push("config.v2.json");
    Ok(base)
}

#[tauri::command]
fn load_local_config() -> Result<Value, String> {
    let path = resolve_default_config_path()?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let default_config = default_local_config();
        let content = serde_json::to_string_pretty(&default_config)
            .map_err(|error| format!("序列化默认配置失败：{error}"))?;
        fs::write(&path, content).map_err(io_error)?;
        return Ok(default_config);
    }

    let content = fs::read_to_string(&path).map_err(io_error)?;
    let mut parsed: Value =
        serde_json::from_str(&content).map_err(|error| format!("解析配置文件失败：{error}"))?;
    if upgrade_legacy_skill_rules_in_place(&mut parsed) {
        let normalized = serde_json::to_string_pretty(&parsed)
            .map_err(|error| format!("序列化配置失败：{error}"))?;
        fs::write(&path, normalized).map_err(io_error)?;
    }
    Ok(parsed)
}

#[tauri::command]
fn save_local_config(config: Value) -> Result<(), String> {
    let path = resolve_default_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let content = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("序列化配置失败：{error}"))?;
    fs::write(&path, content).map_err(io_error)?;
    Ok(())
}

#[tauri::command]
fn get_config_path() -> Result<String, String> {
    let path = resolve_default_config_path()?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn focus_main_window_for_skill_confirmation(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口不存在".to_string())?;
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    let _ = window.request_user_attention(Some(tauri::UserAttentionType::Critical));
    Ok(())
}

fn has_skill_md_in_directory(root: &Path) -> Result<bool, String> {
    let entries = fs::read_dir(root).map_err(io_error)?;
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        let file_type = entry.file_type().map_err(io_error)?;
        if !file_type.is_file() {
            continue;
        }
        let is_skill_md = entry
            .file_name()
            .to_str()
            .map(|name| name.eq_ignore_ascii_case("SKILL.md"))
            .unwrap_or(false);
        if is_skill_md {
            return Ok(true);
        }
    }
    Ok(false)
}

#[tauri::command]
fn pick_folder_dialog(start_dir: Option<String>) -> Result<Option<String>, String> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(path) = start_dir {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            let dir = PathBuf::from(trimmed);
            if dir.exists() {
                dialog = dialog.set_directory(dir);
            }
        }
    }

    Ok(dialog
        .pick_folder()
        .map(|path| path.to_string_lossy().to_string()))
}

#[tauri::command]
fn validate_skill_directory(path: String) -> Result<SkillDirectoryValidation, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok(SkillDirectoryValidation {
            exists: false,
            is_dir: false,
            has_skill_md: false,
        });
    }

    let target = PathBuf::from(trimmed);
    if !target.exists() {
        return Ok(SkillDirectoryValidation {
            exists: false,
            is_dir: false,
            has_skill_md: false,
        });
    }

    if !target.is_dir() {
        return Ok(SkillDirectoryValidation {
            exists: true,
            is_dir: false,
            has_skill_md: false,
        });
    }

    let has_skill_md = has_skill_md_in_directory(&target)?;
    Ok(SkillDirectoryValidation {
        exists: true,
        is_dir: true,
        has_skill_md,
    })
}

fn default_local_config() -> Value {
    let mut cfg = GatewayConfig::default();
    cfg.security.mcp.enabled = false;
    cfg.security.mcp.token.clear();
    cfg.security.admin.enabled = false;
    cfg.security.admin.token.clear();
    cfg.servers.clear();
    serde_json::to_value(cfg).expect("default local config should serialize")
}

fn io_error(error: io::Error) -> String {
    format!("文件操作失败：{error}")
}

#[tauri::command]
fn get_default_skill_rules() -> Vec<SkillCommandRule> {
    GatewayConfig::default().skills.policy.rules
}

#[tauri::command]
async fn test_mcp_server_local(server: ServerConfig) -> Result<Value, String> {
    if server.command.trim().is_empty() {
        return Err("命令不能为空，无法执行检测".to_string());
    }

    let defaults = tokio::task::spawn_blocking(|| match resolve_default_config_path() {
        Ok(path) if path.exists() => load_config_from_path(&path)
            .map(|cfg| cfg.defaults)
            .unwrap_or_else(|_| GatewayConfig::default().defaults),
        _ => GatewayConfig::default().defaults,
    })
    .await
    .map_err(|error| format!("读取默认配置任务失败：{error}"))?;

    match ProcessManager::new().test_server(&server, &defaults).await {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Some(value) = auth_required_test_result(&error.to_string()) {
                Ok(value)
            } else {
                Err(format!("MCP 连通性检测失败：{error}"))
            }
        }
    }
}

fn auth_required_test_result(message: &str) -> Option<Value> {
    let auth_url = extract_auth_field(message, "authorize_url=")?;
    let browser_opened = extract_auth_field(message, "browser_opened=")
        .map(|value| value == "true")
        .unwrap_or(false);
    let waiting_for_authorization = extract_auth_field(message, "waiting_for_authorization=")
        .map(|value| value == "true")
        .unwrap_or(true);

    Some(json!({
        "ok": false,
        "status": "auth_required",
        "message": "需要浏览器登录后再继续检测",
        "authUrl": auth_url,
        "browserOpened": browser_opened,
        "waitingForAuthorization": waiting_for_authorization,
        "testedAt": Utc::now(),
    }))
}

fn extract_auth_field(message: &str, key: &str) -> Option<String> {
    let start = message.find(key)? + key.len();
    let rest = &message[start..];
    let end = rest.find(';').unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

fn upgrade_legacy_skill_rules_in_place(config: &mut Value) -> bool {
    let Some(rules) = config
        .pointer("/skills/policy/rules")
        .and_then(Value::as_array)
    else {
        return false;
    };

    let mut ids = Vec::with_capacity(rules.len());
    for rule in rules {
        let Some(id) = rule.get("id").and_then(Value::as_str) else {
            return false;
        };
        ids.push(id.to_ascii_lowercase());
    }

    let legacy_three = ["deny-rm-root", "confirm-rm", "confirm-remove-item"];
    let legacy_four = [
        "deny-rm-root",
        "deny-remove-item-root",
        "confirm-rm",
        "confirm-remove-item",
    ];
    let is_legacy_pack =
        matches_rule_id_set(&ids, &legacy_three) || matches_rule_id_set(&ids, &legacy_four);
    if !is_legacy_pack {
        return false;
    }

    let default_cfg = GatewayConfig::default();
    let default_rules = serde_json::to_value(default_cfg.skills.policy.rules)
        .expect("default skill rules should serialize");
    if let Some(slot) = config.pointer_mut("/skills/policy/rules") {
        *slot = default_rules;
    }

    // Keep old files consistent with gateway-core default execution limit.
    if let Some(slot) = config.pointer_mut("/skills/execution/maxOutputBytes") {
        if slot.as_u64() == Some(13_107_200) {
            *slot = Value::from(default_cfg.skills.execution.max_output_bytes as u64);
        }
    }
    true
}

fn matches_rule_id_set(ids: &[String], expected: &[&str]) -> bool {
    if ids.len() != expected.len() {
        return false;
    }
    let actual_set: HashSet<&str> = ids.iter().map(|item| item.as_str()).collect();
    let expected_set: HashSet<&str> = expected.iter().copied().collect();
    actual_set == expected_set
}

#[cfg(test)]
mod tests {
    use super::upgrade_legacy_skill_rules_in_place;
    use serde_json::json;

    #[test]
    fn upgrades_legacy_three_rule_pack() {
        let mut cfg = json!({
            "skills": {
                "policy": {
                    "rules": [
                        { "id": "deny-rm-root" },
                        { "id": "confirm-rm" },
                        { "id": "confirm-remove-item" }
                    ]
                },
                "execution": {
                    "maxOutputBytes": 13107200
                }
            }
        });

        let changed = upgrade_legacy_skill_rules_in_place(&mut cfg);
        assert!(changed);
        let rules = cfg["skills"]["policy"]["rules"]
            .as_array()
            .expect("rules should be array");
        assert!(rules.len() > 10);
        assert_eq!(cfg["skills"]["execution"]["maxOutputBytes"], 131072);
    }

    #[test]
    fn keeps_custom_rules_untouched() {
        let mut cfg = json!({
            "skills": {
                "policy": {
                    "rules": [
                        { "id": "deny-rm-root" },
                        { "id": "custom-confirm-delete" }
                    ]
                },
                "execution": {
                    "maxOutputBytes": 13107200
                }
            }
        });

        let changed = upgrade_legacy_skill_rules_in_place(&mut cfg);
        assert!(!changed);
        let rules = cfg["skills"]["policy"]["rules"]
            .as_array()
            .expect("rules should be array");
        assert_eq!(rules.len(), 2);
        assert_eq!(cfg["skills"]["execution"]["maxOutputBytes"], 13107200);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(GatewayProcessState::default())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            gateway_status,
            start_gateway,
            stop_gateway,
            load_local_config,
            save_local_config,
            get_config_path,
            get_default_skill_rules,
            test_mcp_server_local,
            focus_main_window_for_skill_confirmation,
            pick_folder_dialog,
            validate_skill_directory
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
