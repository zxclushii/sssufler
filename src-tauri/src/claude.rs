#[cfg(windows)]
mod pty;
#[cfg(windows)]
pub(crate) use pty::Sessions;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::config::Config;

pub(crate) const LEGACY_ARGS: &str = "--model haiku --disallowedTools Bash Edit Write NotebookEdit";

/// Quotes group words. Backslashes in ordinary Windows paths are literal.
fn split_args(raw: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(|q| Some(*q) == quote && *q == '"') {
            word.push(chars.next().context("ожидалась кавычка")?);
        } else if Some(c) == quote {
            quote = None;
        } else if quote.is_none() && matches!(c, '"' | '\'') {
            quote = Some(c);
            started = true;
        } else if quote.is_none() && c.is_whitespace() {
            if started {
                args.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            started = true;
            word.push(c);
        }
    }
    if quote.is_some() {
        bail!("незакрытая кавычка в claudeArgs");
    }
    if started {
        args.push(word);
    }
    Ok(args)
}

pub(crate) fn safe_args(raw: &str) -> Result<(Vec<String>, Vec<String>)> {
    let mut input = split_args(raw)?.into_iter().peekable();
    let mut args = Vec::new();
    let mut warnings = Vec::new();
    while let Some(token) = input.next() {
        let (flag, inline) = token
            .split_once('=')
            .map_or((token.as_str(), false), |(f, _)| (f, true));
        let key = flag
            .trim_start_matches('-')
            .replace('-', "")
            .to_ascii_lowercase();
        let arity = match key.as_str() {
            "tools" | "allowedtools" | "disallowedtools" | "mcpconfig" | "adddir" => {
                Some(usize::MAX)
            }
            "permissionmode"
            | "settings"
            | "systemprompt"
            | "systempromptfile"
            | "appendsystemprompt"
            | "appendsystempromptfile" => Some(1),
            "dangerouslyskippermissions" | "strictmcpconfig" => Some(0),
            _ => None,
        };
        if flag == "--" {
            warnings.push("claudeArgs: отброшен разделитель -- и его хвост".into());
            break;
        }
        if let Some(n) = arity.filter(|_| flag.starts_with("--")) {
            warnings.push(format!("claudeArgs: параметром {flag} управляет Суфлёр"));
            for _ in 0..n.saturating_sub(usize::from(inline)) {
                if input.next_if(|v| !v.starts_with('-')).is_none() {
                    break;
                }
            }
        } else {
            args.push(token);
        }
    }
    Ok((args, warnings))
}

#[cfg(any(windows, test))]
pub(super) struct Launch {
    pub(super) program: String,
    pub(super) args: Vec<String>,
    pub(super) escape: bool,
}

fn safe_cmd_value(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() || "&|<>^%!\"".contains(c))
}

fn launch(exe: &Path, cfg: &Config, prompt: &Path, system: &Path) -> Result<Launch> {
    let (mut args, _) = safe_args(&cfg.claude_args)?;
    args.extend([
        "--strict-mcp-config".into(),
        "--append-system-prompt-file".into(),
        prompt
            .to_str()
            .context("путь к промпту не в Unicode")?
            .into(),
        "--tools".into(),
    ]);
    if cfg.web_search {
        args.extend([
            "WebSearch".into(),
            "--allowedTools".into(),
            "WebSearch".into(),
        ]);
    } else {
        args.push(String::new());
    }
    let file = exe.to_str().context("путь к claude не в Unicode")?;
    if exe
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"))
    {
        if !safe_cmd_value(file) || args.iter().any(|v| !safe_cmd_value(v)) {
            bail!("путь или аргумент содержит недопустимый для claude.cmd символ");
        }
        let command = std::iter::once(file)
            .chain(args.iter().map(String::as_str))
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(" ");
        return Ok(Launch {
            program: format!("\"{}\"", system.join("System32").join("cmd.exe").display()),
            args: vec![
                "/d".into(),
                "/s".into(),
                "/c".into(),
                format!("\"{command}\""),
            ],
            escape: false,
        });
    }
    Ok(Launch {
        program: format!("\"{file}\""),
        args,
        escape: true,
    })
}

#[cfg(windows)]
fn resolve(command: &str) -> Result<PathBuf> {
    let command = command.trim().trim_matches('"');
    let path = Path::new(command);
    let allowed = |p: &Path| {
        p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("cmd"))
    };
    if path.is_absolute() && allowed(path) && path.is_file() {
        return Ok(path.into());
    }
    if !path.is_absolute() && path.components().count() == 1 {
        let dirs = std::env::var_os("PATH").unwrap_or_default();
        let names = if allowed(path) {
            vec![path.to_path_buf()]
        } else {
            vec![path.with_extension("exe"), path.with_extension("cmd")]
        };
        for name in names {
            for dir in std::env::split_paths(&dirs).filter(|d| d.is_absolute()) {
                let candidate = dir.join(&name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    bail!("claude не найден, укажи путь в настройках")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_policy_is_last_and_empty_is_an_argument() {
        let path = Path::new("C:/program files/claude.exe");
        let mut cfg = Config::default();
        let make =
            |cfg: &Config| launch(path, cfg, Path::new("prompt"), Path::new("Windows")).unwrap();
        let spec = make(&cfg);
        assert_eq!(spec.program, "\"C:/program files/claude.exe\"");
        assert!(spec.escape);
        assert!(spec.args.ends_with(&["--tools".into(), String::new()]));
        cfg.web_search = true;
        assert!(make(&cfg).args.ends_with(&[
            "--tools".into(),
            "WebSearch".into(),
            "--allowedTools".into(),
            "WebSearch".into()
        ]));
    }
    #[test]
    fn batch_has_one_command_and_no_user_metacharacters() {
        let mut cfg = Config::default();
        let make = |cfg: &Config| {
            launch(
                Path::new("C:/program files/claude.cmd"),
                cfg,
                Path::new("a b/prompt"),
                Path::new("C:/Windows"),
            )
        };
        let spec = make(&cfg).unwrap();
        assert!(!spec.escape);
        assert_eq!(&spec.args[..3], &["/d", "/s", "/c"]);
        assert!(spec.args[3].starts_with("\"\"C:/program files/claude.cmd\""));
        assert!(spec.args[3].ends_with("\"--tools\" \"\"\""));
        for c in "&|<>^%!\"".chars() {
            cfg.claude_args = format!("--model 'one{c}two'");
            assert!(make(&cfg).is_err(), "{c}");
        }
    }
    #[test]
    fn removes_managed_flags_and_their_values() {
        let (args,w) = safe_args("--model \"my model\" --allowed-tools Bash Edit --disallowedTools=Read Write --permission-mode ask --settings cfg --strict-mcp-config --append-system-prompt-file file --verbose").unwrap();
        assert_eq!(args, ["--model", "my model", "--verbose"]);
        assert_eq!(w.len(), 6);
        assert!(safe_args("--model 'unfinished").is_err());
    }
}
