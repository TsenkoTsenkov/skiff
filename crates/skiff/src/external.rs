//! Running `kubectl` in the user's own terminal app (iTerm, Terminal,
//! Windows Terminal, ...).
//!
//! The built-in terminal is always there; this is for people who would
//! rather have shells and log tails in the terminal they already live in,
//! with its search, its colours and its key bindings.
//!
//! What is run is built as an argument list ([`Invocation`]) and only then
//! turned into text for one particular shell. Context, namespace, pod and
//! container names are not trusted: a kubeconfig is free text and a cluster
//! can answer with any name, so none of them may ever be read as shell
//! syntax.
//!
//! - macOS and Linux: the command is written, quoted for POSIX `sh`, into a
//!   script only this user can read, in a fresh private folder, and the
//!   terminal is asked to run just `/bin/sh <that path>`. The user's own
//!   interactive shell (which may be fish, with other quoting rules) never
//!   sees a name.
//! - Windows: Windows Terminal runs `kubectl` directly, with no shell in
//!   between; without it, a `cmd` window runs a line quoted for `cmd`.
//!   Arguments `cmd` has no way to quote (`"`, `%`, `!`, control
//!   characters) are refused. `KUBECONFIG` travels in the environment.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// Quotes one word for a POSIX shell.
pub fn sh_quote(word: &str) -> String {
    if !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./:=@,+%".contains(&b)) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// What `cmd` and Windows Terminal cannot be given safely.
const WIN_REFUSED: &str = "this name has characters a Windows terminal cannot be given safely";

/// Quotes one argument for a `cmd` line whose program splits its command
/// line the usual Windows way (as `kubectl` does).
///
/// Inside double quotes `cmd` takes `& | < > ^ ( )` literally, so those are
/// neutralised by quoting. It still expands `%NAME%` (and `!NAME!`) there
/// and has no escape for a `"`, so arguments with `"`, `%`, `!` or control
/// characters are refused. Backslashes before the closing quote are
/// doubled, as the Windows argument rules want.
pub fn win_quote(arg: &str) -> Result<String, String> {
    if arg.chars().any(|c| matches!(c, '"' | '%' | '!') || c.is_control()) {
        return Err(WIN_REFUSED.into());
    }
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:\\@+=,".contains(c)) {
        return Ok(arg.to_string());
    }
    let trailing = arg.chars().rev().take_while(|c| *c == '\\').count();
    Ok(format!("\"{arg}{}\"", "\\".repeat(trailing)))
}

/// One or more `kubectl` commands to run side by side in one terminal
/// window, as argument lists.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Invocation {
    /// Each command in full, program name first.
    pub commands: Vec<Vec<String>>,
    /// `KUBECONFIG` for the commands (a new terminal window does not
    /// always inherit this app's environment).
    pub kubeconfig: Option<String>,
    /// The commands end by themselves (logs that are not followed), so the
    /// window has to stay open for the output to be read.
    pub finite: bool,
}

/// `kubectl --context <ctx>`.
fn kubectl(context: &str) -> Vec<String> {
    vec!["kubectl".into(), "--context".into(), context.into()]
}

/// A pod name as an argument that can never be read as an option. (No real
/// pod name starts with `-`, but the name comes from the cluster.)
fn pod_arg(pod: &str) -> String {
    if pod.starts_with('-') {
        format!("pod/{pod}")
    } else {
        pod.to_string()
    }
}

/// The kubeconfig files the app reads when they are not the kubectl
/// default (see [`set_kubeconfig`]).
static KUBECONFIG: Mutex<Option<String>> = Mutex::new(None);

/// Tells this module which kubeconfig files the app is using, as a
/// `KUBECONFIG` value (a path list), so that the `kubectl` commands it
/// builds see the same clusters. `None`: the kubectl default (the
/// environment's `KUBECONFIG`, if any).
pub fn set_kubeconfig(path_list: Option<String>) {
    if let Ok(mut kc) = KUBECONFIG.lock() {
        *kc = path_list.filter(|p| !p.is_empty());
    }
}

fn env_kubeconfig() -> Option<String> {
    KUBECONFIG
        .lock()
        .ok()
        .and_then(|kc| kc.clone())
        .or_else(|| std::env::var("KUBECONFIG").ok().filter(|kc| !kc.is_empty()))
}

/// A shell in a container.
pub fn exec_invocation(context: &str, ns: &str, pod: &str, container: Option<&str>) -> Invocation {
    let mut argv = kubectl(context);
    argv.extend(["-n".into(), ns.into(), "exec".into(), "-it".into(), pod_arg(pod)]);
    if let Some(c) = container {
        argv.extend(["-c".into(), c.into()]);
    }
    argv.extend(["--", "sh", "-c", "command -v bash >/dev/null 2>&1 && exec bash || exec sh"].map(String::from));
    Invocation { commands: vec![argv], kubeconfig: env_kubeconfig(), finite: false }
}

/// Following the logs of the given `(namespace, pod, container)`s. Several
/// containers are followed side by side, each line prefixed with its source.
pub fn logs_invocation(
    context: &str,
    targets: &[(String, String, String)],
    tail: Option<i64>,
    previous: bool,
    timestamps: bool,
) -> Invocation {
    let commands = targets
        .iter()
        .map(|(ns, pod, container)| {
            let mut argv = kubectl(context);
            argv.extend(["-n".into(), ns.clone(), "logs".into(), pod_arg(pod), "-c".into(), container.clone()]);
            argv.push(if previous { "--previous".into() } else { "-f".into() });
            argv.push(match tail {
                Some(n) => format!("--tail={n}"),
                None => "--tail=-1".into(),
            });
            if timestamps {
                argv.push("--timestamps".into());
            }
            if targets.len() > 1 {
                argv.push("--prefix".into());
            }
            argv
        })
        .collect();
    Invocation { commands, kubeconfig: env_kubeconfig(), finite: previous }
}

impl Invocation {
    /// The same commands with another `KUBECONFIG` (what the app actually
    /// loaded, when that is not `$KUBECONFIG`).
    pub fn with_kubeconfig(mut self, kubeconfig: Option<String>) -> Self {
        self.kubeconfig = kubeconfig.filter(|kc| !kc.is_empty());
        self
    }

    /// The command line for `sh`, bash and zsh: every argument quoted, the
    /// commands joined with `&` and waited for.
    pub fn posix_command(&self) -> String {
        let env = self.kubeconfig.as_deref().map(|kc| format!("KUBECONFIG={} ", sh_quote(kc))).unwrap_or_default();
        let parts: Vec<String> = self
            .commands
            .iter()
            .map(|argv| format!("{env}{}", argv.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")))
            .collect();
        match parts.len() {
            0 => String::new(),
            1 => parts.into_iter().next().unwrap_or_default(),
            _ => format!("{} & wait", parts.join(" & ")),
        }
    }

    /// The command line for `cmd` (one command also works pasted into
    /// PowerShell, unless a name contains `$` or a backtick). All commands
    /// but the last are started in the background of the same window.
    /// `KUBECONFIG` is not part of it: it goes through the environment.
    /// Fails when an argument cannot be quoted for `cmd` ([`win_quote`]).
    pub fn windows_command(&self) -> Result<String, String> {
        let mut parts: Vec<String> = Vec::new();
        for (i, argv) in self.commands.iter().enumerate() {
            let words = argv.iter().map(|a| win_quote(a)).collect::<Result<Vec<_>, _>>()?.join(" ");
            let last = i + 1 == self.commands.len();
            // (`start`'s first quoted word is a window title.)
            parts.push(if last { words } else { format!("start \"\" /b {words}") });
        }
        Ok(parts.join(" & "))
    }

    /// The arguments for Windows Terminal (`wt.exe`), which starts each
    /// command itself, one tab each: no shell reads the names. `None` when
    /// this cannot be given to it: `;` separates its own commands, the
    /// arguments [`win_quote`] refuses are refused here as well, so is one
    /// that Windows Terminal would quote wrongly, and a tab
    /// closes as soon as its command ends well, which would hide the output
    /// of a command that just prints and ends.
    pub fn wt_args(&self) -> Option<Vec<String>> {
        if self.finite || self.commands.is_empty() {
            return None;
        }
        let mut args: Vec<String> = Vec::new();
        for argv in &self.commands {
            // Windows Terminal joins the arguments into one command line
            // again: it puts quotes around one with a space and escapes
            // nothing, so a backslash at the end of such an argument would
            // swallow the closing quote and what follows.
            let breaks_out = |a: &str| a.contains(' ') && a.ends_with('\\');
            if argv.is_empty()
                || argv.iter().any(|a| a.is_empty() || a.contains(';') || breaks_out(a) || win_quote(a).is_err())
            {
                return None;
            }
            if !args.is_empty() {
                args.push(";".into());
            }
            args.push("new-tab".into());
            args.extend(argv.iter().cloned());
        }
        Some(args)
    }

    /// The command line to show and copy on this platform.
    pub fn display(&self) -> String {
        if cfg!(windows) {
            // (`rem`: pasted anyway, this does nothing.)
            self.windows_command().unwrap_or_else(|e| format!("rem {e}"))
        } else {
            self.posix_command()
        }
    }
}

/// The command line for a shell in a container, for this platform's shell.
pub fn exec_command(context: &str, ns: &str, pod: &str, container: Option<&str>) -> String {
    exec_invocation(context, ns, pod, container).display()
}

/// The command line that follows the logs of the given containers, for
/// this platform's shell.
pub fn logs_command(
    context: &str,
    targets: &[(String, String, String)],
    tail: Option<i64>,
    previous: bool,
    timestamps: bool,
) -> String {
    logs_invocation(context, targets, tail, previous, timestamps).display()
}

/// The terminal app asked for in the config file, if any.
static PREFERRED: Mutex<Option<String>> = Mutex::new(None);

/// Sets the terminal app to use (the config file's `"terminal"`): a name
/// such as `iTerm`, `Ghostty` or `kitty`, or on Linux a program path.
/// `$SKIFF_TERMINAL` still wins over it.
pub fn set_preferred_terminal(name: Option<&str>) {
    if let Ok(mut p) = PREFERRED.lock() {
        *p = name.map(str::trim).filter(|n| !n.is_empty()).map(String::from);
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn preferred_terminal() -> Option<String> {
    std::env::var("SKIFF_TERMINAL")
        .ok()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| PREFERRED.lock().ok().and_then(|p| p.clone()))
}

/// Opens a new window of the user's terminal app running the commands.
/// Returns the name of the terminal app that was used.
pub fn open_invocation(inv: &Invocation) -> Result<String, String> {
    if inv.commands.iter().all(|c| c.is_empty()) {
        return Err("there is nothing to run".into());
    }
    open_invocation_impl(inv)
}

/// Opens a new window of the user's terminal app running `command`, which
/// must be a line made by [`exec_command`] or [`logs_command`] (it is run
/// as it is). Returns the name of the terminal app that was used.
/// [`open_invocation`] is the better way: on Windows only that one can use
/// Windows Terminal and pass on another `KUBECONFIG`.
pub fn open(command: &str) -> Result<String, String> {
    if command.trim().is_empty() {
        return Err("there is nothing to run".into());
    }
    open_line(command)
}

/// The script a terminal runs with `/bin/sh`. It first removes itself (the
/// shell has it open already), then runs `line` with this app's `PATH`
/// added after the terminal's own, so that `kubectl` and the credential
/// helpers are found whichever is the fuller one. With `keep_open` the
/// user's shell follows, so that the window stays and the last words can
/// be read.
pub fn script_text(line: &str, own_dir: Option<&str>, path: Option<&str>, keep_open: bool) -> String {
    let mut s = String::from("#!/bin/sh\n");
    if let Some(dir) = own_dir {
        s.push_str(&format!("rm -rf -- {}\n", sh_quote(dir)));
    }
    if let Some(path) = path.filter(|p| !p.is_empty()) {
        s.push_str(&format!("PATH=\"${{PATH:+$PATH:}}\"{}; export PATH\n", sh_quote(path)));
    }
    s.push_str(line);
    s.push('\n');
    if keep_open {
        s.push_str("exec \"${SHELL:-/bin/sh}\"\n");
    }
    s
}

/// Whether `path` can be typed into any shell as it is: no character that
/// any of them could read as syntax.
#[cfg_attr(not(unix), allow(dead_code))]
fn plain_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|s| s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)))
}

/// Writes a script only this user can read or run, in a new folder only
/// this user can enter, and returns its path. `text` is given the folder.
/// Nothing that already exists is reused or followed.
#[cfg(unix)]
fn private_script(text: &dyn Fn(&Path) -> String) -> Result<std::path::PathBuf, String> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    let tmp = std::env::temp_dir();
    // The path is typed into the user's shell by some terminals, so it
    // must need no quoting at all.
    let base = if plain_path(&tmp) { tmp } else { "/tmp".into() };
    let dir = base.join(format!("skiff-term-{:016x}", crate::shell_env::random()));
    let fail = |e: std::io::Error| format!("could not prepare the command in {}: {e}", base.display());
    // (Not recursive: fails if the path exists, whatever it is.)
    std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(fail)?;
    let path = dir.join("run.sh");
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&path)
        .and_then(|mut f| f.write_all(text(&dir).as_bytes()));
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(fail(e));
    }
    Ok(path)
}

/// Writes the script for `line` and hands its path to `launch`; removes it
/// again if no terminal took it.
#[cfg(unix)]
fn open_line(line: &str) -> Result<String, String> {
    if line.contains('\0') {
        return Err("this name cannot be passed to a terminal".into());
    }
    let path_var = std::env::var("PATH").ok();
    // Terminal and iTerm type into a shell that stays; elsewhere the
    // script is the window's only program.
    let keep_open =
        !cfg!(target_os = "macos") || !matches!(mac_terminal().0, MacTerminal::ITerm | MacTerminal::Terminal);
    let script = private_script(&|dir| script_text(line, dir.to_str(), path_var.as_deref(), keep_open))?;
    let script_str = script.to_str().unwrap_or_default().to_string();
    let result = launch(&script_str);
    if result.is_err() {
        if let Some(dir) = script.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    result
}

#[cfg(unix)]
fn open_invocation_impl(inv: &Invocation) -> Result<String, String> {
    if inv.commands.iter().flatten().any(|a| a.contains('\0')) {
        return Err("this name cannot be passed to a terminal".into());
    }
    open_line(&inv.posix_command())
}

/// The terminal apps known on macOS.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum MacTerminal {
    ITerm,
    Terminal,
    Ghostty,
    WezTerm,
    Kitty,
    Alacritty,
}

/// Which macOS terminal to use: the one asked for by name, else iTerm when
/// it is installed, else Terminal.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn pick_mac_terminal(preferred: Option<&str>, has_iterm: bool) -> MacTerminal {
    let p = preferred.unwrap_or("").to_lowercase();
    if p.contains("iterm") {
        MacTerminal::ITerm
    } else if p.contains("ghostty") {
        MacTerminal::Ghostty
    } else if p.contains("wezterm") {
        MacTerminal::WezTerm
    } else if p.contains("kitty") {
        MacTerminal::Kitty
    } else if p.contains("alacritty") {
        MacTerminal::Alacritty
    } else if p.is_empty() && has_iterm {
        MacTerminal::ITerm
    } else {
        // (Also for "Terminal", "Apple Terminal" and names not known.)
        MacTerminal::Terminal
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_terminal() -> (MacTerminal, &'static str) {
    let home = std::env::var("HOME").unwrap_or_default();
    let has_app = |name: &str| {
        Path::new(&format!("/Applications/{name}.app")).exists()
            || Path::new(&format!("{home}/Applications/{name}.app")).exists()
    };
    let t = pick_mac_terminal(preferred_terminal().as_deref(), has_app("iTerm"));
    let name = match t {
        MacTerminal::ITerm => "iTerm",
        MacTerminal::Terminal => "Terminal",
        MacTerminal::Ghostty => "Ghostty",
        MacTerminal::WezTerm => "WezTerm",
        MacTerminal::Kitty => "kitty",
        MacTerminal::Alacritty => "Alacritty",
    };
    (t, name)
}

/// The program and arguments that make a macOS terminal run the script at
/// `script` (a path that needs no quoting, see [`plain_path`]).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_launch(terminal: MacTerminal, script: &str) -> Vec<String> {
    fn applescript_string(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
    let osascript = |source: String| vec!["/usr/bin/osascript".to_string(), "-e".to_string(), source];
    let open_app = |app: &str, args: &[&str]| {
        let mut v: Vec<String> = ["/usr/bin/open", "-na", app, "--args"].map(String::from).to_vec();
        v.extend(args.iter().map(|a| a.to_string()));
        v.extend(["/bin/sh".to_string(), script.to_string()]);
        v
    };
    // Typed into the window's shell: only the fixed `/bin/sh` and a path
    // without any special character.
    let typed = applescript_string(&format!("/bin/sh {script}"));
    match terminal {
        MacTerminal::ITerm => osascript(format!(
            "tell application \"iTerm\"\n\
             activate\n\
             set w to (create window with default profile)\n\
             tell current session of w to write text {typed}\n\
             end tell"
        )),
        MacTerminal::Terminal => {
            osascript(format!("tell application \"Terminal\"\nactivate\ndo script {typed}\nend tell"))
        }
        MacTerminal::Ghostty => open_app("Ghostty", &["-e"]),
        MacTerminal::WezTerm => open_app("WezTerm", &["start", "--"]),
        MacTerminal::Kitty => open_app("kitty", &[]),
        MacTerminal::Alacritty => open_app("Alacritty", &["-e"]),
    }
}

#[cfg(target_os = "macos")]
fn launch(script: &str) -> Result<String, String> {
    if !plain_path(Path::new(script)) {
        return Err("could not prepare the command: the temporary folder has an unusual path".into());
    }
    let (terminal, app) = mac_terminal();
    let argv = mac_launch(terminal, script);
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not open {app}: {e}"))?;
    if out.status.success() {
        Ok(app.to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!("{app} did not open: {err}"))
    }
}

/// How a Linux terminal program is told to run `/bin/sh <script>`.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn terminal_args(name: &str, script: &str) -> Vec<String> {
    let before: &[&str] = match name {
        "gnome-terminal" | "ptyxis" => &["--"],
        "wezterm" => &["start", "--"],
        "kitty" | "foot" | "footclient" => &[],
        "terminator" | "xfce4-terminal" | "mate-terminal" => &["-x"],
        // xterm, x-terminal-emulator, alacritty, ghostty, konsole, kgx,
        // tilix, lxterminal, ...
        _ => &["-e"],
    };
    before.iter().map(|a| a.to_string()).chain(["/bin/sh".to_string(), script.to_string()]).collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn launch(script: &str) -> Result<String, String> {
    let mut candidates: Vec<String> = Vec::new();
    candidates.extend(preferred_terminal());
    candidates.extend(std::env::var("TERMINAL").ok().filter(|t| !t.trim().is_empty()));
    candidates.extend(
        [
            "x-terminal-emulator",
            "kitty",
            "wezterm",
            "alacritty",
            "ghostty",
            "ptyxis",
            "kgx",
            "gnome-terminal",
            "konsole",
            "xfce4-terminal",
            "tilix",
            "terminator",
            "mate-terminal",
            "lxterminal",
            "foot",
            "xterm",
        ]
        .map(String::from),
    );
    for term in candidates {
        let term = term.trim();
        // A full path as it is; a name only from the absolute folders of
        // `PATH`.
        let program = if term.starts_with('/') {
            std::path::PathBuf::from(term)
        } else {
            match crate::shell_env::which(term) {
                Some(p) => p,
                None => continue,
            }
        };
        let name = program.file_name().and_then(|n| n.to_str()).unwrap_or(term).to_string();
        let spawned = Command::new(&program)
            .args(terminal_args(&name, script))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return Ok(name);
        }
    }
    Err("No terminal app was found. Set \"terminal\" in Skiff's config file, or $TERMINAL.".into())
}

/// `cmd.exe` by its full path.
#[cfg(windows)]
fn cmd_exe() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    std::path::PathBuf::from(root).join("System32").join("cmd.exe")
}

/// A new `cmd` window that runs `line` and stays open. `line` is passed as
/// it is: it must have been made by [`Invocation::windows_command`].
#[cfg(windows)]
fn open_cmd(line: &str, kubeconfig: Option<&str>) -> Result<String, String> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    if line.chars().any(|c| c.is_control()) {
        return Err(WIN_REFUSED.into());
    }
    let mut cmd = Command::new(cmd_exe());
    // No `start` and no second `cmd` in between: the line is read once.
    cmd.arg("/k").raw_arg(line).creation_flags(CREATE_NEW_CONSOLE);
    if let Some(kc) = kubeconfig {
        cmd.env("KUBECONFIG", kc);
    }
    cmd.spawn().map(|_| "cmd".to_string()).map_err(|e| format!("could not open a terminal: {e}"))
}

#[cfg(windows)]
fn open_line(line: &str) -> Result<String, String> {
    open_cmd(line, None)
}

#[cfg(windows)]
fn open_invocation_impl(inv: &Invocation) -> Result<String, String> {
    // (Checks every argument, whichever terminal is used.)
    let line = inv.windows_command()?;
    if let (Some(wt), Some(args)) = (crate::shell_env::which("wt.exe"), inv.wt_args()) {
        let mut cmd = Command::new(wt);
        // (The standard library quotes each argument for the program.)
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(kc) = &inv.kubeconfig {
            cmd.env("KUBECONFIG", kc);
        }
        if cmd.spawn().is_ok() {
            return Ok("Windows Terminal".to_string());
        }
    }
    open_cmd(&line, inv.kubeconfig.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names nobody chose kindly.
    const HOSTILE: [&str; 12] = [
        "a'; touch PWNED; '",
        r"\'; touch PWNED #",
        "$(touch PWNED)",
        "`touch PWNED`",
        "a & touch PWNED",
        "a | b > PWNED < c ^ d",
        "50%PATH%",
        "say \"hi\" && touch PWNED",
        "!boom!",
        "two\nlines; touch PWNED",
        "-o=jsonpath",
        "",
    ];

    fn no_env(mut inv: Invocation) -> Invocation {
        inv.kubeconfig = None;
        inv
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("my-pod_1.x"), "my-pod_1.x");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn posix_commands() {
        let c = no_env(exec_invocation("prod", "default", "web-1", Some("app"))).posix_command();
        assert_eq!(
            c,
            "kubectl --context prod -n default exec -it web-1 -c app -- sh -c \
             'command -v bash >/dev/null 2>&1 && exec bash || exec sh'"
        );
        let targets = vec![
            ("ns".to_string(), "a".to_string(), "c".to_string()),
            ("ns".to_string(), "b".to_string(), "c".to_string()),
        ];
        let l = no_env(logs_invocation("prod", &targets, Some(100), false, false)).posix_command();
        assert!(l.contains("logs a -c c -f --tail=100 --prefix & "));
        assert!(l.ends_with("& wait"));
        let one = no_env(logs_invocation("prod", &targets[..1], None, true, true));
        assert!(one.posix_command().ends_with("logs a -c c --previous --tail=-1 --timestamps"));
        assert!(one.finite);
        let with = one.with_kubeconfig(Some("/home/me/my kube/config".into())).posix_command();
        assert!(with.starts_with("KUBECONFIG='/home/me/my kube/config' kubectl --context prod "));
        assert_eq!(Invocation::default().posix_command(), "");
    }

    #[test]
    fn a_pod_name_is_never_an_option() {
        let inv = exec_invocation("c", "ns", "--kubeconfig=/evil", None);
        assert_eq!(inv.commands[0][7], "pod/--kubeconfig=/evil");
        let inv = logs_invocation("c", &[("ns".into(), "-f".into(), "c".into())], None, false, false);
        assert_eq!(inv.commands[0][6], "pod/-f");
    }

    /// The real thing: the script a terminal is given, run by `/bin/sh`
    /// with a `kubectl` that writes down the arguments it got.
    #[cfg(unix)]
    #[test]
    fn hostile_names_reach_kubectl_unchanged_through_the_script() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("skiff-ext-test-{:016x}", crate::shell_env::random()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("kubectl");
        std::fs::write(&fake, "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\"; done >>\"$OUT\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        for (i, name) in HOSTILE.iter().enumerate() {
            let out = dir.join(format!("out-{i}"));
            let inv = exec_invocation(name, name, name, Some(name)).with_kubeconfig(Some(format!("/k/{name}")));
            let script = dir.join(format!("run-{i}.sh"));
            let text = script_text(&inv.posix_command(), None, bin.to_str(), false);
            std::fs::write(&script, text).unwrap();
            let status = Command::new("/bin/sh")
                .arg(&script)
                .current_dir(&dir)
                .env("OUT", &out)
                .env("PATH", "/nonexistent")
                .status()
                .unwrap();
            assert!(status.success(), "{name:?}");
            let got = std::fs::read(&out).unwrap();
            let got: Vec<&str> = got.split(|b| *b == 0).map(|a| std::str::from_utf8(a).unwrap()).collect();
            let pod = if name.starts_with('-') { format!("pod/{name}") } else { name.to_string() };
            let want = [
                "--context",
                name,
                "-n",
                name,
                "exec",
                "-it",
                &pod,
                "-c",
                name,
                "--",
                "sh",
                "-c",
                "command -v bash >/dev/null 2>&1 && exec bash || exec sh",
                "",
            ];
            assert_eq!(got, want, "{name:?}");
        }
        // Nothing but the fake kubectl ran.
        assert!(!dir.join("PWNED").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_script_is_private_and_removes_itself() {
        use std::os::unix::fs::PermissionsExt as _;
        let script = private_script(&|dir| script_text("echo ran >/dev/null", dir.to_str(), None, false)).unwrap();
        let dir = script.parent().unwrap().to_path_buf();
        assert!(plain_path(&script), "{script:?}");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(Command::new("/bin/sh").arg(&script).status().unwrap().success());
        assert!(!dir.exists());
    }

    #[test]
    fn script_text_layout() {
        let s = script_text("kubectl get pods", Some("/tmp/skiff-term-1"), Some("/opt/my bin:/usr/bin"), true);
        assert_eq!(
            s,
            "#!/bin/sh\n\
             rm -rf -- /tmp/skiff-term-1\n\
             PATH=\"${PATH:+$PATH:}\"'/opt/my bin:/usr/bin'; export PATH\n\
             kubectl get pods\n\
             exec \"${SHELL:-/bin/sh}\"\n"
        );
        assert_eq!(script_text("x", None, Some(""), false), "#!/bin/sh\nx\n");
    }

    // (Only unix types a script path into a shell.)
    #[cfg(unix)]
    #[test]
    fn only_plain_paths_are_typed_into_a_shell() {
        assert!(plain_path(Path::new("/var/folders/x_1/T/skiff-term-0a/run.sh")));
        for bad in ["/tmp/a b/run.sh", "/tmp/a'b", "/tmp/$(x)", "/tmp/a;b", "/tmp/a\\b", "relative/run.sh", ""] {
            assert!(!plain_path(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn windows_quoting() {
        assert_eq!(win_quote("web-1").unwrap(), "web-1");
        assert_eq!(win_quote("my context").unwrap(), "\"my context\"");
        assert_eq!(win_quote("").unwrap(), "\"\"");
        // cmd operators are literal inside quotes.
        assert_eq!(win_quote("a&b").unwrap(), "\"a&b\"");
        assert_eq!(win_quote("a | b > c < d ^ e").unwrap(), "\"a | b > c < d ^ e\"");
        assert_eq!(win_quote("(x);y,z=1").unwrap(), "\"(x);y,z=1\"");
        // POSIX quoting characters mean nothing to cmd, but are kept whole.
        assert_eq!(win_quote("it's").unwrap(), "\"it's\"");
        assert_eq!(win_quote("$(calc)").unwrap(), "\"$(calc)\"");
        // A backslash before the closing quote must not escape it.
        assert_eq!(win_quote(r"C:\Users\me\.kube\config").unwrap(), r"C:\Users\me\.kube\config");
        assert_eq!(win_quote(r"C:\my dir\").unwrap(), r#""C:\my dir\\""#);
        // What cmd cannot quote at all is refused.
        for bad in ["a\"b", "50%PATH%", "!x!", "two\nlines", "tab\there", "\"", "a\rb", "nul\0"] {
            assert!(win_quote(bad).is_err(), "{bad:?}");
        }
    }

    /// Splits a `cmd` line the way cmd does: at `&`, `|`, `<`, `>` outside
    /// double quotes. A correctly quoted line has no such place.
    fn cmd_operators_outside_quotes(line: &str) -> usize {
        let mut quoted = false;
        let mut n = 0;
        for c in line.chars() {
            match c {
                '"' => quoted = !quoted,
                '&' | '|' | '<' | '>' | '^' if !quoted => n += 1,
                _ => {}
            }
        }
        assert!(!quoted, "unbalanced quotes in {line}");
        n
    }

    #[test]
    fn windows_commands() {
        let c = exec_invocation("prod", "default", "web-1", Some("app")).windows_command().unwrap();
        assert_eq!(
            c,
            "kubectl --context prod -n default exec -it web-1 -c app -- sh -c \
             \"command -v bash >/dev/null 2>&1 && exec bash || exec sh\""
        );
        assert_eq!(cmd_operators_outside_quotes(&c), 0);
        // No POSIX syntax: no single-quoted words, no KUBECONFIG= prefix.
        let c = exec_invocation("my ctx", "ns", "p", None)
            .with_kubeconfig(Some(r"C:\k\a;C:\k\b".into()))
            .windows_command()
            .unwrap();
        assert!(c.starts_with("kubectl --context \"my ctx\" -n ns exec -it p -- sh"));
        assert!(!c.contains("KUBECONFIG") && !c.contains('\''));

        let targets = vec![
            ("ns".to_string(), "a".to_string(), "c".to_string()),
            ("ns".to_string(), "b".to_string(), "c".to_string()),
        ];
        let l = logs_invocation("prod", &targets, Some(5), false, false).windows_command().unwrap();
        assert_eq!(
            l,
            "start \"\" /b kubectl --context prod -n ns logs a -c c -f --tail=5 --prefix & \
             kubectl --context prod -n ns logs b -c c -f --tail=5 --prefix"
        );
    }

    #[test]
    fn hostile_names_on_windows_are_quoted_or_refused() {
        for name in HOSTILE {
            let inv = exec_invocation(name, name, name, Some(name));
            let refused = name.chars().any(|c| matches!(c, '"' | '%' | '!') || c.is_control());
            match inv.windows_command() {
                Ok(line) => {
                    assert!(!refused, "{name:?} must be refused");
                    // The only operators are inside quotes, so cmd runs
                    // exactly one program: kubectl.
                    assert_eq!(cmd_operators_outside_quotes(&line), 0, "{line}");
                    assert!(line.starts_with("kubectl --context "));
                    assert!(!line.contains('%') && !line.contains('!'));
                }
                Err(e) => {
                    assert!(refused, "{name:?} should be quotable");
                    assert_eq!(e, WIN_REFUSED);
                    assert!(inv.wt_args().is_none());
                }
            }
        }
        // The text shown for a refused name is harmless when pasted.
        assert!(WIN_REFUSED.chars().all(|c| c.is_ascii_alphanumeric() || c == ' '));
    }

    #[test]
    fn windows_terminal_arguments() {
        let targets = vec![
            ("ns".to_string(), "a".to_string(), "c".to_string()),
            ("ns".to_string(), "b&calc".to_string(), "c".to_string()),
        ];
        let args = logs_invocation("my ctx", &targets, None, false, false).wt_args().unwrap();
        let want = "new-tab kubectl --context|my ctx|-n ns logs a -c c -f --tail=-1 --prefix ; \
                    new-tab kubectl --context|my ctx|-n ns logs b&calc -c c -f --tail=-1 --prefix";
        // (`|` here only to show where one argument has a space.)
        assert_eq!(args.join(" ").replace(" my ctx ", "|my ctx|"), want);
        // `;` would start another Windows Terminal command.
        assert!(exec_invocation("a;b", "ns", "p", None).wt_args().is_none());
        assert!(exec_invocation("ctx", "ns", "p ; new-tab calc", None).wt_args().is_none());
        assert!(exec_invocation("ctx", "", "p", None).wt_args().is_none());
        // A space and a backslash at the end: Windows Terminal's own
        // quoting would let the rest of the line out of the quotes. Such a
        // name goes through cmd, where it is quoted properly.
        let odd = "a b\\";
        let evil = "c --kubeconfig=\\\\evil\\share\\cfg exec x d\\";
        for inv in [
            exec_invocation(odd, "ns", "p", Some("c")),
            exec_invocation("ctx", odd, "p", Some("c")),
            exec_invocation("ctx", "ns", "p", Some(odd)),
            exec_invocation("ctx", odd, "p", Some(evil)),
            logs_invocation("ctx", &[("ns".to_string(), "p".to_string(), odd.to_string())], None, false, false),
        ] {
            assert!(inv.wt_args().is_none());
            assert!(inv.windows_command().unwrap().contains("\"a b\\\\\""));
        }
        // Without a space nothing is quoted, and the backslash is plain.
        assert!(exec_invocation("ctx\\", "ns", "p", None).wt_args().is_some());
        // Finished logs must stay readable: a cmd window, not a tab.
        assert!(logs_invocation("c", &targets[..1], None, true, false).wt_args().is_none());
        assert!(Invocation::default().wt_args().is_none());
    }

    #[test]
    fn mac_terminals() {
        assert_eq!(pick_mac_terminal(None, true), MacTerminal::ITerm);
        assert_eq!(pick_mac_terminal(None, false), MacTerminal::Terminal);
        assert_eq!(pick_mac_terminal(Some("Terminal"), true), MacTerminal::Terminal);
        assert_eq!(pick_mac_terminal(Some("iTerm2"), false), MacTerminal::ITerm);
        assert_eq!(pick_mac_terminal(Some("ghostty"), true), MacTerminal::Ghostty);
        assert_eq!(pick_mac_terminal(Some("WezTerm"), true), MacTerminal::WezTerm);

        let script = "/var/folders/ab/T/skiff-term-01/run.sh";
        let t = mac_launch(MacTerminal::Terminal, script);
        assert_eq!(t[..2], ["/usr/bin/osascript", "-e"]);
        assert!(t[2].contains("do script \"/bin/sh /var/folders/ab/T/skiff-term-01/run.sh\""));
        let i = mac_launch(MacTerminal::ITerm, script);
        assert!(i[2].contains("write text \"/bin/sh /var/folders/ab/T/skiff-term-01/run.sh\""));
        assert_eq!(
            mac_launch(MacTerminal::WezTerm, script),
            ["/usr/bin/open", "-na", "WezTerm", "--args", "start", "--", "/bin/sh", script]
        );
        assert_eq!(
            mac_launch(MacTerminal::Ghostty, script),
            ["/usr/bin/open", "-na", "Ghostty", "--args", "-e", "/bin/sh", script]
        );
    }

    #[test]
    fn linux_terminals_run_only_sh_and_the_script() {
        assert_eq!(terminal_args("gnome-terminal", "/t/run.sh"), ["--", "/bin/sh", "/t/run.sh"]);
        assert_eq!(terminal_args("wezterm", "/t/run.sh"), ["start", "--", "/bin/sh", "/t/run.sh"]);
        assert_eq!(terminal_args("kitty", "/t/run.sh"), ["/bin/sh", "/t/run.sh"]);
        assert_eq!(terminal_args("xterm", "/t/run.sh"), ["-e", "/bin/sh", "/t/run.sh"]);
        assert_eq!(terminal_args("xfce4-terminal", "/t/run.sh"), ["-x", "/bin/sh", "/t/run.sh"]);
    }

    #[test]
    fn preferred_terminal_can_be_set() {
        set_preferred_terminal(Some("  kitty "));
        assert_eq!(PREFERRED.lock().unwrap().as_deref(), Some("kitty"));
        set_preferred_terminal(Some(" "));
        assert_eq!(*PREFERRED.lock().unwrap(), None);
    }
}
