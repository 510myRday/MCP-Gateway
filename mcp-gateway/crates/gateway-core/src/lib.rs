pub mod config;
pub mod error;
pub mod runtime;
pub mod terminal;

pub use config::{
    apply_runtime_overrides, default_config_path, generate_token, init_default_config,
    load_config_from_path, migrate_v1_to_v2_file, normalize_config_in_place, rotate_token,
    save_config_atomic, validate_config, ConfigService, DefaultsConfig, GatewayConfig,
    LifecycleMode, RunMode, ServerConfig, SkillCommandRule, SkillPolicyAction, SkillsConfig,
    SkillsExecutionConfig, SkillsPathGuardConfig, SkillsPolicyConfig, StdioProtocol, TokenConfig,
    TokenScope, TransportConfig,
};
pub use error::{AppError, ErrorCode};
pub use runtime::{AuthOrchestrator, AuthSessionStatus, ProcessManager, ServerAuthState};
pub use terminal::{
    detect_terminal_encoding_status, is_powershell_like_command,
    wrap_windows_powershell_command_for_utf8, TerminalEncodingStatus,
};
