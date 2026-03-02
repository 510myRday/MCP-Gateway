use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;
use std::{fs, io};

use chrono::Utc;
use gateway_core::{load_config_from_path, ConfigService, ProcessManager};
use gateway_http::{build_router, spawn_idle_reaper, AppState, SseHub};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const EMBEDDED_RUNTIME_ID: &str = "embedded://gateway-runtime";

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

    ensure_listen_port_available(&cfg.listen)?;

    let config_service = ConfigService::from_path(config_path.clone())
        .await
        .map_err(|error| format!("初始化配置服务失败：{error}"))?;

    let state = AppState {
        config_service,
        process_manager: ProcessManager::new(),
        started_at: Utc::now(),
        sse_hub: SseHub::new(),
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
    let output = Command::new("cmd")
        .args(["/C", &format!(
            "for /f \"tokens=5\" %a in ('netstat -ano ^| findstr :{port} ^| findstr LISTENING') do taskkill /F /PID %a"
        )])
        .output()
        .map_err(|e| format!("执行端口清理命令失败: {e}"))?;

    if output.status.success() {
        Ok(format!("已清理端口 {port} 的占用进程"))
    } else {
        // 也尝试 PowerShell 方式
        let ps_output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!(
                "Get-NetTCPConnection -LocalPort {port} -State Listen -ErrorAction SilentlyContinue | ForEach-Object {{ Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }}"
            )])
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
    let parsed: Value =
        serde_json::from_str(&content).map_err(|error| format!("解析配置文件失败：{error}"))?;
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

fn default_local_config() -> Value {
    json!({
        "version": 2,
        "listen": "127.0.0.1:8765",
        "allowNonLoopback": false,
        "mode": "both",
        "security": {
            "mcp": {
                "enabled": false,
                "token": ""
            },
            "admin": {
                "enabled": false,
                "token": ""
            }
        },
        "transport": {
            "streamableHttp": {
                "basePath": "/api/v2/mcp"
            },
            "sse": {
                "basePath": "/api/v2/sse"
            }
        },
        "defaults": {
            "lifecycle": "pooled",
            "idleTtlMs": 300000,
            "requestTimeoutMs": 60000,
            "maxRetries": 2,
            "maxResponseWaitIterations": 100
        },
        "servers": []
    })
}

fn io_error(error: io::Error) -> String {
    format!("文件操作失败：{error}")
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
            get_config_path
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
