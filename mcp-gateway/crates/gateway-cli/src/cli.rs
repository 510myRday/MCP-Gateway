use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use gateway_core::{RunMode, TokenScope};

#[derive(Debug, Parser)]
#[command(name = "gateway", version, about = "MCP Gateway V2")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        mode: Option<RunModeArg>,
        #[arg(long)]
        listen: Option<String>,
    },
    Init {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = RunModeArg::Both)]
        mode: RunModeArg,
        #[arg(long)]
        force: bool,
    },
    Validate {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    MigrateConfig {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, value_name = "INPUT")]
        input: PathBuf,
        #[arg(long, value_name = "OUTPUT")]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    Rotate {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum)]
        scope: TokenScopeArg,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RunModeArg {
    Extension,
    General,
    Both,
}

impl From<RunModeArg> for RunMode {
    fn from(value: RunModeArg) -> Self {
        match value {
            RunModeArg::Extension => RunMode::Extension,
            RunModeArg::General => RunMode::General,
            RunModeArg::Both => RunMode::Both,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TokenScopeArg {
    Admin,
    Mcp,
}

impl From<TokenScopeArg> for TokenScope {
    fn from(value: TokenScopeArg) -> Self {
        match value {
            TokenScopeArg::Admin => TokenScope::Admin,
            TokenScopeArg::Mcp => TokenScope::Mcp,
        }
    }
}
