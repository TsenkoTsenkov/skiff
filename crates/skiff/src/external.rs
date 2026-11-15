//! Running `kubectl` in the user's own terminal app (iTerm, Terminal, ...).
//!
//! The built-in terminal is always there; this is for people who would
//! rather have shells and log tails in the terminal they already live in,
//! with its search, its colours and its key bindings.

use std::process::{Command, Stdio};

/// Quotes one word for a POSIX shell.
pub fn sh_quote(word: &str) -> String {
    if !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./:=@,+%".contains(&b)) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// `kubectl --context <ctx>` plus `KUBECONFIG=` when the app was given one
/// (a new terminal window does not inherit this app's environment).
fn kubectl(context: &str) -> String {
    let mut cmd = String::new();
    // The files chosen in Settings, else whatever this app was started with.
    if let Some(kc) = skiff_core::kubeconfig::kubeconfig_env_value().or_else(|| std::env::var("KUBECONFIG").ok()) {
        if !kc.is_empty() {
            cmd.push_str(&format!("KUBECONFIG={} ", sh_quote(&kc)));
        }
    }
    cmd.push_str(&format!("kubectl --context {}", sh_quote(context)));
    cmd
}

/// The command line for a shell in a container.
pub fn exec_command(context: &str, ns: &str, pod: &str, container: Option<&str>) -> String {
    let mut cmd = format!("{} -n {} exec -it {}", kubectl(context), sh_quote(ns), sh_quote(pod));
    if let Some(c) = container {
        cmd.push_str(&format!(" -c {}", sh_quote(c)));
    }
    cmd.push_str(" -- sh -c 'command -v bash >/dev/null 2>&1 && exec bash || exec sh'");
    cmd
}

/// The command line that follows the logs of the given containers. Several
/// containers are followed side by side, each line prefixed with its source.
pub fn logs_command(
    context: &str,
    targets: &[(String, String, String)],
    tail: Option<i64>,
    previous: bool,
    timestamps: bool,
) -> String {
    let parts: Vec<String> = targets
        .iter()
        .map(|(ns, pod, container)| {
            let mut c = format!(
                "{} -n {} logs {} -c {}",
                kubectl(context),
                sh_quote(ns),
                sh_quote(pod),
                sh_quote(container)
            );
            if previous {
                c.push_str(" --previous");
            } else {
                c.push_str(" -f");
            }
            match tail {
                Some(n) => c.push_str(&format!(" --tail={n}")),
                None => c.push_str(" --tail=-1"),
            }
            if timestamps {
                c.push_str(" --timestamps");
            }
            if targets.len() > 1 {
                c.push_str(" --prefix");
            }
            c
        })
        .collect();
    match parts.len() {
        0 => String::new(),
        1 => parts.into_iter().next().unwrap_or_default(),
        _ => format!("{} & wait", parts.join(" & ")),
    }
}

/// Opens a new window of the user's terminal app running `command` in their
/// login shell (so that `kubectl` and credential plugins are on `PATH`).
/// Returns the name of the terminal app that was used.
pub fn open(command: &str) -> Result<String, String> {
    open_impl(command)
}

#[cfg(target_os = "macos")]
fn open_impl(command: &str) -> Result<String, String> {
    fn applescript_string(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let has_app = |name: &str| {
        std::path::Path::new(&format!("/Applications/{name}.app")).exists()
            || std::path::Path::new(&format!("{home}/Applications/{name}.app")).exists()
    };
    let preferred = std::env::var("SKIFF_TERMINAL").unwrap_or_default().to_lowercase();
    let iterm = if preferred.is_empty() { has_app("iTerm") } else { preferred.contains("iterm") };
    let cmd = applescript_string(command);
    let (app, script) = if iterm {
        (
            "iTerm",
            format!(
                "tell application \"iTerm\"\n\
                 activate\n\
                 set w to (create window with default profile)\n\
                 tell current session of w to write text {cmd}\n\
                 end tell"
            ),
        )
    } else {
        ("Terminal", format!("tell application \"Terminal\"\nactivate\ndo script {cmd}\nend tell"))
    };
    let status = Command::new("osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run osascript: {e}"))?;
    if status.status.success() {
        Ok(app.to_string())
    } else {
        let err = String::from_utf8_lossy(&status.stderr).trim().to_string();
        Err(format!("{app} did not open: {err}"))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_impl(command: &str) -> Result<String, String> {
    // Keep the window open after the command ends, so its last words can be read.
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let script = format!("{command}; exec {}", sh_quote(&shell));
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(t) = std::env::var("SKIFF_TERMINAL").or_else(|_| std::env::var("TERMINAL")) {
        if !t.is_empty() {
            candidates.push(t);
        }
    }
    for t in [
        "x-terminal-emulator",
        "kitty",
        "wezterm",
        "alacritty",
        "ghostty",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "foot",
        "xterm",
    ] {
        candidates.push(t.to_string());
    }
    for term in candidates {
        let name = std::path::Path::new(&term).file_name().and_then(|n| n.to_str()).unwrap_or(&term).to_string();
        let mut cmd = Command::new(&term);
        match name.as_str() {
            "gnome-terminal" => cmd.args(["--", &shell, "-lc", &script]),
            "wezterm" => cmd.args(["start", "--", &shell, "-lc", &script]),
            "kitty" | "foot" => cmd.args([&shell, "-lc", &script]),
            _ => cmd.args(["-e", &shell, "-lc", &script]),
        };
        if cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok() {
            return Ok(name);
        }
    }
    Err("no terminal app found (set $TERMINAL or $SKIFF_TERMINAL)".into())
}

#[cfg(windows)]
fn open_impl(command: &str) -> Result<String, String> {
    Command::new("cmd")
        .args(["/c", "start", "", "cmd", "/k", command])
        .spawn()
        .map(|_| "cmd".to_string())
        .map_err(|e| format!("could not open a terminal: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("my-pod_1.x"), "my-pod_1.x");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn commands() {
        let c = exec_command("prod", "default", "web-1", Some("app"));
        assert!(c.contains("--context prod -n default exec -it web-1 -c app -- sh -c"));
        let targets = vec![
            ("ns".to_string(), "a".to_string(), "c".to_string()),
            ("ns".to_string(), "b".to_string(), "c".to_string()),
        ];
        let l = logs_command("prod", &targets, Some(100), false, false);
        assert!(l.contains("logs a -c c -f --tail=100 --prefix & "));
        assert!(l.ends_with("& wait"));
        let one = logs_command("prod", &targets[..1], None, true, true);
        assert!(one.ends_with("logs a -c c --previous --tail=-1 --timestamps"));
    }
}
