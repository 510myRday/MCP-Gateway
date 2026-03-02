use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

use crate::config::StdioProtocol;
use crate::error::AppError;

pub async fn write_message(
    stdin: &mut ChildStdin,
    message: &serde_json::Value,
    protocol: &StdioProtocol,
) -> Result<(), AppError> {
    let body = serde_json::to_vec(message)?;
    match protocol {
        StdioProtocol::Auto | StdioProtocol::ContentLength => {
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            stdin.write_all(header.as_bytes()).await?;
            stdin.write_all(&body).await?;
            stdin.flush().await?;
        }
        StdioProtocol::JsonLines => {
            stdin.write_all(&body).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }
    }
    Ok(())
}

pub async fn read_message(
    stdout: &mut BufReader<ChildStdout>,
    protocol: &StdioProtocol,
) -> Result<serde_json::Value, AppError> {
    match protocol {
        StdioProtocol::Auto | StdioProtocol::ContentLength => {
            read_message_content_length(stdout).await
        }
        StdioProtocol::JsonLines => read_message_json_lines(stdout).await,
    }
}

pub async fn read_message_content_length(
    stdout: &mut BufReader<ChildStdout>,
) -> Result<serde_json::Value, AppError> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let bytes = stdout.read_line(&mut line).await?;
        if bytes == 0 {
            return Err(AppError::Upstream(
                "stdio process closed output while waiting for response".to_string(),
            ));
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue;
        }

        if let Some(length) = parse_content_length_header(trimmed)? {
            content_length = Some(length);
            continue;
        }

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return Ok(value);
        }
    }

    let length = content_length
        .ok_or_else(|| AppError::Upstream("missing Content-Length header".to_string()))?;
    read_content_length_payload(stdout, length).await
}

pub async fn read_message_json_lines(
    stdout: &mut BufReader<ChildStdout>,
) -> Result<serde_json::Value, AppError> {
    loop {
        let mut line = String::new();
        let bytes = stdout.read_line(&mut line).await?;
        if bytes == 0 {
            return Err(AppError::Upstream(
                "stdio process closed output while waiting for response".to_string(),
            ));
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return Ok(value);
        }

        if let Some(length) = parse_content_length_header(trimmed)? {
            loop {
                let mut header_line = String::new();
                let bytes = stdout.read_line(&mut header_line).await?;
                if bytes == 0 {
                    return Err(AppError::Upstream(
                        "stdio process closed output while reading Content-Length frame headers"
                            .to_string(),
                    ));
                }
                if header_line.trim().is_empty() {
                    break;
                }
            }
            return read_content_length_payload(stdout, length).await;
        }
    }
}

pub fn parse_content_length_header(line: &str) -> Result<Option<usize>, AppError> {
    let lower = line.to_ascii_lowercase();
    let Some(raw) = lower.strip_prefix("content-length:") else {
        return Ok(None);
    };

    let length = raw
        .trim()
        .parse::<usize>()
        .map_err(|_| AppError::Upstream(format!("parse Content-Length header: {line}")))?;

    Ok(Some(length))
}

async fn read_content_length_payload(
    stdout: &mut BufReader<ChildStdout>,
    length: usize,
) -> Result<serde_json::Value, AppError> {
    let mut body = vec![0_u8; length];
    stdout.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(AppError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_header_case_insensitive() {
        assert_eq!(
            parse_content_length_header("Content-Length: 42").expect("parse"),
            Some(42)
        );
        assert_eq!(
            parse_content_length_header("content-length: 7").expect("parse"),
            Some(7)
        );
    }

    #[test]
    fn parses_non_header_as_none() {
        assert_eq!(
            parse_content_length_header("{\"jsonrpc\":\"2.0\"}").expect("parse"),
            None
        );
    }
}
