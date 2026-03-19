#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
use std::process::Command;

use serde::{Deserialize, Serialize};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "windows")]
const UTF8_CODE_PAGE: u32 = 65001;

#[cfg(target_os = "windows")]
const POWERSHELL_UTF8_PRELUDE: &str = "$__mcpGatewayUtf8 = New-Object System.Text.UTF8Encoding($false); [Console]::InputEncoding = $__mcpGatewayUtf8; [Console]::OutputEncoding = $__mcpGatewayUtf8; $OutputEncoding = $__mcpGatewayUtf8; $PSDefaultParameterValues['*:Encoding'] = 'utf8'; chcp 65001 > $null";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEncodingStatus {
    pub shell: String,
    pub detected: bool,
    pub is_utf8: bool,
    pub code_page: Option<u32>,
    pub input_code_page: Option<u32>,
    pub output_code_page: Option<u32>,
    pub auto_fix_on_launch: bool,
}

pub fn detect_terminal_encoding_status() -> TerminalEncodingStatus {
    #[cfg(target_os = "windows")]
    {
        detect_windows_powershell_encoding_status()
    }

    #[cfg(not(target_os = "windows"))]
    {
        TerminalEncodingStatus {
            shell: "system".to_string(),
            detected: true,
            is_utf8: true,
            code_page: None,
            input_code_page: None,
            output_code_page: None,
            auto_fix_on_launch: false,
        }
    }
}

#[cfg(target_os = "windows")]
pub fn wrap_windows_powershell_command_for_utf8(
    executable: &str,
    args: &[String],
) -> Option<(String, Vec<String>)> {
    if !is_powershell_like_command(executable) {
        return None;
    }

    if let Some(injected_args) = inject_utf8_prelude_into_command_args(args) {
        return Some((executable.to_string(), injected_args));
    }

    let quoted_executable = powershell_single_quoted(executable);
    let invocation = if args.is_empty() {
        format!("& {quoted_executable}")
    } else {
        let quoted_args = args
            .iter()
            .map(|arg| powershell_single_quoted(arg))
            .collect::<Vec<_>>()
            .join(", ");
        format!("& {quoted_executable} @({quoted_args})")
    };
    let script = format!("{POWERSHELL_UTF8_PRELUDE}; {invocation}");

    Some((
        executable.to_string(),
        vec![
            "-NoProfile".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-Command".to_string(),
            script,
        ],
    ))
}

#[cfg(not(target_os = "windows"))]
pub fn wrap_windows_powershell_command_for_utf8(
    _executable: &str,
    _args: &[String],
) -> Option<(String, Vec<String>)> {
    None
}

pub fn is_powershell_like_command(command: &str) -> bool {
    let trimmed = command.trim().trim_matches(|ch| matches!(ch, '"' | '\''));
    let file_name = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .trim_matches(|ch| matches!(ch, '"' | '\''))
        .to_ascii_lowercase();

    matches!(
        file_name.as_str(),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    )
}

#[cfg(target_os = "windows")]
fn inject_utf8_prelude_into_command_args(args: &[String]) -> Option<Vec<String>> {
    let command_index = args
        .iter()
        .position(|arg| arg.eq_ignore_ascii_case("-command") || arg.eq_ignore_ascii_case("-c"))?;
    let command_text = args.get(command_index + 1)?;

    let mut injected = args.to_vec();
    injected[command_index + 1] = format!("{POWERSHELL_UTF8_PRELUDE}; {command_text}");
    Some(injected)
}

#[cfg(target_os = "windows")]
fn powershell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(target_os = "windows")]
fn detect_windows_powershell_encoding_status() -> TerminalEncodingStatus {
    const PROBE_SCRIPT: &str = "$probe = [ordered]@{ shell = 'powershell'; codePage = [Console]::OutputEncoding.CodePage; inputCodePage = [Console]::InputEncoding.CodePage; outputCodePage = if ($OutputEncoding) { $OutputEncoding.CodePage } else { $null } }; $probe | ConvertTo-Json -Compress";

    let mut command = Command::new("powershell");
    command.args(["-NoProfile", "-Command", PROBE_SCRIPT]);
    command.creation_flags(CREATE_NO_WINDOW);

    let Ok(output) = command.output() else {
        return TerminalEncodingStatus {
            shell: "powershell".to_string(),
            detected: false,
            is_utf8: false,
            code_page: None,
            input_code_page: None,
            output_code_page: None,
            auto_fix_on_launch: false,
        };
    };

    if !output.status.success() {
        return TerminalEncodingStatus {
            shell: "powershell".to_string(),
            detected: false,
            is_utf8: false,
            code_page: None,
            input_code_page: None,
            output_code_page: None,
            auto_fix_on_launch: false,
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_windows_encoding_probe_output(&stdout).unwrap_or(TerminalEncodingStatus {
        shell: "powershell".to_string(),
        detected: false,
        is_utf8: false,
        code_page: None,
        input_code_page: None,
        output_code_page: None,
        auto_fix_on_launch: false,
    })
}

#[cfg(target_os = "windows")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsEncodingProbe {
    shell: Option<String>,
    code_page: Option<u32>,
    input_code_page: Option<u32>,
    output_code_page: Option<u32>,
}

#[cfg(target_os = "windows")]
fn parse_windows_encoding_probe_output(output: &str) -> Option<TerminalEncodingStatus> {
    let trimmed = output.trim().trim_start_matches('\u{feff}');
    if trimmed.is_empty() {
        return None;
    }

    let probe: WindowsEncodingProbe = serde_json::from_str(trimmed).ok()?;
    let code_page = probe.code_page.or(probe.output_code_page);
    let input_code_page = probe.input_code_page;
    let output_code_page = probe.output_code_page;
    let is_utf8 = code_page == Some(UTF8_CODE_PAGE)
        && input_code_page == Some(UTF8_CODE_PAGE)
        && output_code_page.unwrap_or(UTF8_CODE_PAGE) == UTF8_CODE_PAGE;

    Some(TerminalEncodingStatus {
        shell: probe.shell.unwrap_or_else(|| "powershell".to_string()),
        detected: true,
        is_utf8,
        code_page,
        input_code_page,
        output_code_page,
        auto_fix_on_launch: !is_utf8,
    })
}

#[cfg(test)]
mod tests {
    use super::{is_powershell_like_command, wrap_windows_powershell_command_for_utf8};

    #[cfg(target_os = "windows")]
    use super::parse_windows_encoding_probe_output;

    #[test]
    fn identifies_powershell_commands() {
        assert!(is_powershell_like_command("powershell"));
        assert!(is_powershell_like_command("pwsh.exe"));
        assert!(is_powershell_like_command(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        ));
        assert!(!is_powershell_like_command("python"));
    }

    #[test]
    fn wraps_powershell_command_for_utf8_on_windows() {
        let wrapped = wrap_windows_powershell_command_for_utf8(
            "powershell",
            &[String::from("-Command"), String::from("Write-Host '中文'")],
        );

        if cfg!(target_os = "windows") {
            let (command, args) = wrapped.expect("powershell should be wrapped on windows");
            assert_eq!(command, "powershell");
            assert_eq!(args[0], "-Command");
            assert!(args[1].contains("chcp 65001"));
            assert!(args[1].contains("$PSDefaultParameterValues['*:Encoding'] = 'utf8'"));
            assert!(args[1].contains("Write-Host '中文'"));
        } else {
            assert!(wrapped.is_none());
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn parses_windows_encoding_probe_output() {
        let parsed = parse_windows_encoding_probe_output(
            r#"{"shell":"powershell","codePage":936,"inputCodePage":936,"outputCodePage":65001}"#,
        )
        .expect("probe output should parse");

        assert_eq!(parsed.shell, "powershell");
        assert_eq!(parsed.code_page, Some(936));
        assert_eq!(parsed.input_code_page, Some(936));
        assert_eq!(parsed.output_code_page, Some(65001));
        assert!(!parsed.is_utf8);
        assert!(parsed.auto_fix_on_launch);
    }
}
