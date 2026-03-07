use serde_json::Value;

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegotiatedStdioProtocol {
    ContentLength,
    JsonLines,
}

pub fn alternate_protocol(protocol: NegotiatedStdioProtocol) -> NegotiatedStdioProtocol {
    match protocol {
        NegotiatedStdioProtocol::ContentLength => NegotiatedStdioProtocol::JsonLines,
        NegotiatedStdioProtocol::JsonLines => NegotiatedStdioProtocol::ContentLength,
    }
}

pub fn protocol_label(protocol: NegotiatedStdioProtocol) -> &'static str {
    match protocol {
        NegotiatedStdioProtocol::ContentLength => "content-length",
        NegotiatedStdioProtocol::JsonLines => "json-lines",
    }
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
    use super::*;

    #[test]
    fn alternate_protocol_switches_between_supported_framings() {
        assert_eq!(
            alternate_protocol(NegotiatedStdioProtocol::ContentLength),
            NegotiatedStdioProtocol::JsonLines
        );
        assert_eq!(
            alternate_protocol(NegotiatedStdioProtocol::JsonLines),
            NegotiatedStdioProtocol::ContentLength
        );
    }
}
