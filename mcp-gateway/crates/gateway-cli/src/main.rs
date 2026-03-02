mod cli;

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use cli::{Cli, Commands, TokenCommand};
use gateway_core::{
    apply_runtime_overrides, default_config_path, init_default_config, load_config_from_path,
    migrate_v1_to_v2_file, rotate_token, save_config_atomic, validate_config, ConfigService,
    ProcessManager, RunMode,
};
use gateway_http::{build_router, spawn_idle_reaper, AppState, SseHub};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init {
            config,
            mode,
            force,
        } => {
            let config_path = resolve_config_path(config)?;
            if config_path.exists() && !force {
                return Err(anyhow!(
                    "config already exists at {} (use --force to overwrite)",
                    config_path.display()
                ));
            }

            let cfg = init_default_config(&config_path, mode.into())?;
            println!("Initialized config: {}", config_path.display());
            println!("Mode: {}", cfg.mode);
            if cfg.security.admin.enabled {
                println!("Admin token: {}", cfg.security.admin.token);
            }
            if cfg.security.mcp.enabled {
                println!("MCP token: {}", cfg.security.mcp.token);
            }
            Ok(())
        }
        Commands::Validate { config } => {
            let config_path = resolve_config_path(config)?;
            let cfg = load_config_from_path(&config_path)
                .with_context(|| format!("failed to load config from {}", config_path.display()))?;
            validate_config(&cfg)?;
            println!("Config is valid: {}", config_path.display());
            Ok(())
        }
        Commands::Token { command } => match command {
            TokenCommand::Rotate { config, scope } => {
                let config_path = resolve_config_path(config)?;
                let token = rotate_token(&config_path, scope.into()).with_context(|| {
                    format!("failed to rotate token in {}", config_path.display())
                })?;
                println!("Rotated token: {}", token);
                Ok(())
            }
        },
        Commands::MigrateConfig {
            from,
            to,
            input,
            output,
        } => {
            if from != "v1" || to != "v2" {
                return Err(anyhow!("only --from v1 --to v2 is currently supported"));
            }
            let cfg = migrate_v1_to_v2_file(&input, &output)?;
            println!(
                "Migrated config from {} to {} with {} servers",
                input.display(),
                output.display(),
                cfg.servers.len()
            );
            Ok(())
        }
        Commands::Run {
            config,
            mode,
            listen,
        } => {
            let config_path = resolve_config_path(config)?;
            if !config_path.exists() {
                let initial_mode = mode.map(Into::into).unwrap_or(RunMode::Both);
                let _ = init_default_config(&config_path, initial_mode).with_context(|| {
                    format!(
                        "failed to create default config at {}",
                        config_path.display()
                    )
                })?;
            }

            let mut cfg = load_config_from_path(&config_path)
                .with_context(|| format!("failed to load config from {}", config_path.display()))?;
            apply_runtime_overrides(&mut cfg, mode.map(Into::into), listen);
            validate_config(&cfg)?;
            save_config_atomic(&config_path, &cfg)?;

            let config_service = ConfigService::from_path(config_path.clone()).await?;
            let process_manager = ProcessManager::new();
            let state = AppState {
                config_service,
                process_manager: process_manager.clone(),
                started_at: chrono::Utc::now(),
                sse_hub: SseHub::new(),
            };

            let app = build_router(state.clone(), &cfg);
            spawn_idle_reaper(state.clone());

            let listener = tokio::net::TcpListener::bind(&cfg.listen)
                .await
                .with_context(|| format!("failed to bind {}", cfg.listen))?;

            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .context("server error")?;

            process_manager.reset_pool().await;
            Ok(())
        }
    }
}

fn resolve_config_path(path: Option<PathBuf>) -> Result<PathBuf> {
    match path {
        Some(value) => Ok(value),
        None => default_config_path().map_err(Into::into),
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            let _ = sigterm.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
