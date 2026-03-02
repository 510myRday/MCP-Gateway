use serde_json::Value;

use crate::config::{ServerConfig, StdioProtocol};
use crate::error::AppError;

pub fn alternate_protocol(protocol: &StdioProtocol) -> StdioProtocol {
    match protocol {
        StdioProtocol::Auto => StdioProtocol::ContentLength,
        StdioProtocol::ContentLength => StdioProtocol::JsonLines,
        StdioProtocol::JsonLines => StdioProtocol::ContentLength,
    }
}

pub fn infer_protocol_from_server(server: &ServerConfig) -> Option<StdioProtocol> {
    let mut merged = server.command.to_ascii_lowercase();
    if !server.args.is_empty() {
        merged.push(' ');
        merged.push_str(&server.args.join(" ").to_ascii_lowercase());
    }

    const JSONL_PATTERNS: &[&str] = &["@modelcontextprotocol/server-filesystem", "@playwright/mcp"];
    if JSONL_PATTERNS
        .iter()
        .any(|pattern| merged.contains(pattern))
    {
        return Some(StdioProtocol::JsonLines);
    }

    None
}

pub fn should_attempt_protocol_fallback(
    request: &Value,
    error: &AppError,
    allow_any_request_fallback: bool,
) -> bool {
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return false;
    };
    if method != "initialize" && !allow_any_request_fallback {
        return false;
    }

    let lower = error.to_string().to_ascii_lowercase();
    lower.contains("request timed out waiting for stdio response")
        || lower.contains("missing content-length header")
        || lower.contains("parse content-length header")
        || lower.contains("parse stdio json payload")
        || lower.contains("closed output while waiting for response")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{LifecycleMode, ServerConfig};

    #[test]
    fn infer_json_lines_known_server() {
        let server = ServerConfig {
            name: "filesystem".to_string(),
            description: String::new(),
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ],
            cwd: String::new(),
            env: HashMap::new(),
            lifecycle: Some(LifecycleMode::Pooled),
            stdio_protocol: StdioProtocol::Auto,
            enabled: true,
        };
        assert_eq!(
            infer_protocol_from_server(&server),
            Some(StdioProtocol::JsonLines)
        );
    }
}
