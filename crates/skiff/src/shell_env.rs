//! The environment of an app that was not started from a terminal.
//!
//! An app opened from the Finder, the Dock or a Linux desktop launcher gets
//! a minimal environment, not the one the user's shell sets up. The
//! credential helpers a kubeconfig calls (`aws`, `gke-gcloud-auth-plugin`,
//! `kubelogin`, ...) are usually only on the shell's `PATH`, and
//! `KUBECONFIG` and the proxy settings usually only exist there too, so
//! without them such clusters fail to connect.
//!
//! Only a fixed, short list of variables is taken over ([`IMPORTED`]), and
//! each only when this process does not have it already: the shell's whole
//! environment (cloud profiles, tokens, ...) is deliberately not imported.
//! Windows needs none of this: there the environment comes from the user's
//! settings however the app is started.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// The variables taken from the login shell besides `PATH`, each only if it
/// is not set (in either spelling) in this process.
pub const IMPORTED: [&str; 7] =
    ["KUBECONFIG", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "NO_PROXY", "no_proxy"];

/// `PATH` made of the shell's entries first, then the current ones, then
/// `extra`, without duplicates and in that order. Entries that are not
/// absolute (`.`, `bin`, an empty entry, which means the current folder)
/// are dropped: helpers must never be looked up relative to wherever the
/// app happens to run.
pub fn merge_paths(current: &OsStr, from_shell: Option<&OsStr>, extra: &[&str]) -> OsString {
    let mut parts: Vec<PathBuf> = Vec::new();
    let shell = from_shell.map(std::env::split_paths).into_iter().flatten();
    let all = shell.chain(std::env::split_paths(current)).chain(extra.iter().map(PathBuf::from));
    for part in all {
        // (A value printed by a shell may end in a newline.)
        let part = match part.to_str() {
            Some(s) => PathBuf::from(s.trim()),
            None => part,
        };
        if part.is_absolute() && !parts.contains(&part) {
            parts.push(part);
        }
    }
    // Joining only fails for an entry containing the separator, which
    // splitting cannot have produced.
    std::env::join_paths(parts).unwrap_or_else(|_| current.to_os_string())
}

/// [`merge_paths`] for text.
pub fn merged_path(current: &str, from_shell: Option<&str>, extra: &[&str]) -> String {
    merge_paths(OsStr::new(current), from_shell.map(OsStr::new), extra).to_string_lossy().into_owned()
}

/// The full path of the program `name`, looked up in the absolute entries
/// of `PATH` only. (`Command::new("name")` would also try relative ones.)
pub fn which(name: &str) -> Option<PathBuf> {
    which_in(&std::env::var_os("PATH")?, name)
}

/// [`which`] for a given `PATH`.
pub fn which_in(path: &OsStr, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return None;
    }
    std::env::split_paths(path).filter(|dir| dir.is_absolute()).map(|dir| dir.join(name)).find(|p| p.is_file())
}

/// The arguments that make `shell` run `script` after reading the files a
/// terminal window would read. zsh, bash and fish also read their
/// interactive file (`.zshrc`, `.bashrc`), where most people set `PATH`.
pub fn shell_args(shell: &str, script: &str) -> Vec<String> {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    let flags: &[&str] = match name {
        "zsh" | "bash" | "fish" => &["-l", "-i", "-c"],
        "sh" | "dash" | "ksh" | "mksh" | "ash" => &["-l", "-c"],
        // (csh/tcsh take -l only on its own; others are unknown.)
        _ => &["-c"],
    };
    flags.iter().map(|f| f.to_string()).chain([script.to_string()]).collect()
}

/// The command a shell is asked to run: the environment between two
/// markers, so that anything the shell's own start-up files print (a
/// greeting, a prompt, a warning) cannot be mistaken for it. Plain enough
/// for sh, zsh, bash and fish. `env -0` separates with NUL, so that values
/// with newlines stay whole; where it does not exist, plain `env`.
pub fn dump_script(marker: &str) -> String {
    format!("printf '%s' '{marker}<'; /usr/bin/env -0 2>/dev/null || /usr/bin/env; printf '%s' '>{marker}'")
}

/// The variables printed by [`dump_script`], or `None` when `out` does not
/// hold a complete dump.
pub fn parse_dump(out: &[u8], marker: &str) -> Option<Vec<(String, String)>> {
    let begin = format!("{marker}<").into_bytes();
    let end = format!(">{marker}").into_bytes();
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).rposition(|w| w == needle);
    let stop = find(out, &end)?;
    let start = find(&out[..stop], &begin)? + begin.len();
    let body = &out[start..stop];
    let sep = if body.contains(&0) { 0u8 } else { b'\n' };
    let mut vars: Vec<(String, String)> = Vec::new();
    for entry in body.split(|b| *b == sep) {
        let Ok(entry) = std::str::from_utf8(entry) else { continue };
        let Some((name, value)) = entry.split_once('=') else { continue };
        // The first one wins: with newline separators a later line may be
        // the middle of some other variable's value.
        if !name.is_empty() && !vars.iter().any(|(n, _)| n == name) {
            vars.push((name.to_string(), value.to_string()));
        }
    }
    Some(vars)
}

/// What to set in this process, given the shell's variables (`None`: the
/// shell was not asked or did not answer) and a way to read our own.
pub fn plan(
    shell: Option<&[(String, String)]>,
    current: &dyn Fn(&str) -> Option<OsString>,
    extra_path: &[&str],
) -> Vec<(String, OsString)> {
    let get = |name: &str| shell?.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
    let mut set: Vec<(String, OsString)> = Vec::new();
    let path_now = current("PATH").unwrap_or_default();
    let merged = merge_paths(&path_now, get("PATH").map(OsStr::new), extra_path);
    if merged != path_now {
        set.push(("PATH".into(), merged));
    }
    for name in IMPORTED {
        // `HTTPS_PROXY` set here beats `https_proxy` from the shell.
        let have = |n: &str| current(n).is_some_and(|v| !v.is_empty());
        if have(name) || have(&name.to_uppercase()) || have(&name.to_lowercase()) {
            continue;
        }
        match get(name) {
            // (Nothing that could not be a real value.)
            Some(v) if !v.trim().is_empty() && !v.contains('\0') => set.push((name.to_string(), v.into())),
            _ => {}
        }
    }
    set
}

/// Asks the user's login shell for its environment. Bounded: a shell that
/// hangs (a start-up file waiting for input) is given up on.
#[cfg(unix)]
fn ask_shell() -> Option<Vec<(String, String)>> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    let fallback = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" };
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/') && std::path::Path::new(s).is_file())
        .unwrap_or_else(|| fallback.to_string());
    let marker = format!("__SKIFF_ENV_{}_{:016x}__", std::process::id(), random());
    let mut child = Command::new(&shell)
        .args(shell_args(&shell, &dump_script(&marker)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread, so that neither a large environment (more than a
    // pipe holds) nor a shell that never exits can block the start. The
    // thread stops at the closing marker: an interactive shell may linger.
    let (tx, rx) = mpsc::channel();
    let end = format!(">{marker}").into_bytes();
    std::thread::spawn(move || {
        let (mut out, mut buf) = (Vec::new(), [0u8; 8192]);
        loop {
            match stdout.read(&mut buf) {
                Ok(n) if n > 0 => {
                    out.extend_from_slice(&buf[..n]);
                    // 4 MiB of start-up chatter is not an environment.
                    if out.windows(end.len()).any(|w| w == end) || out.len() > (4 << 20) {
                        break;
                    }
                }
                _ => break,
            }
        }
        let _ = tx.send(out);
    });
    // A login shell that takes longer than this is not worth delaying the
    // window for.
    let out = rx.recv_timeout(Duration::from_secs(3)).ok();
    let _ = child.kill();
    let _ = child.wait();
    parse_dump(&out?, &marker)
}

/// A number nobody else can predict, without a dependency for it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn random() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.finish()
}

/// Gives this process the `PATH`, `KUBECONFIG` and proxy settings of the
/// user's login shell (see the module text). Call it first thing in `main`,
/// before any other thread exists. Does nothing on Windows.
pub fn adopt_shell_env() {
    #[cfg(unix)]
    {
        use std::io::IsTerminal as _;
        // Started from a terminal, the environment already is the shell's.
        let in_terminal =
            std::io::stdin().is_terminal() || std::io::stdout().is_terminal() || std::io::stderr().is_terminal();
        let from_shell = if in_terminal { None } else { ask_shell() };
        // Where Homebrew and hand-installed tools go, in case the shell
        // did not say.
        let extra: Vec<&str> = ["/opt/homebrew/bin", "/usr/local/bin"]
            .into_iter()
            .filter(|p| (cfg!(target_os = "macos") || *p == "/usr/local/bin") && std::path::Path::new(p).is_dir())
            .collect();
        for (name, value) in plan(from_shell.as_deref(), &|n| std::env::var_os(n), &extra) {
            // SAFETY: called at the very start of `main`. The only other
            // thread that can exist is `ask_shell`'s reader when the shell
            // hung, and that one only reads a pipe: nothing reads or
            // writes the environment concurrently.
            unsafe { std::env::set_var(name, value) };
        }
    }
}

/// The older name of [`adopt_shell_env`].
pub fn adopt_shell_path() {
    adopt_shell_env();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn shell_entries_come_first_and_nothing_repeats() {
        assert_eq!(
            merged_path("/usr/bin:/bin", Some("/opt/homebrew/bin:/usr/bin"), &["/usr/local/bin", "/opt/homebrew/bin"]),
            "/opt/homebrew/bin:/usr/bin:/bin:/usr/local/bin"
        );
        assert_eq!(merged_path("/usr/bin:/bin", None, &[]), "/usr/bin:/bin");
        assert_eq!(merged_path("", Some("/a::/b\n"), &[]), "/a:/b");
    }

    #[test]
    fn relative_path_entries_are_dropped() {
        assert_eq!(merged_path(".:/usr/bin:bin::../x", Some(".:/opt/bin:~/bin"), &["rel"]), "/opt/bin:/usr/bin");
    }

    #[test]
    fn which_only_looks_in_absolute_folders() {
        assert_eq!(which_in(OsStr::new("/nonexistent:/bin:/usr/bin"), "sh").map(|p| p.is_absolute()), Some(true));
        assert_eq!(which_in(OsStr::new("."), "Cargo.toml"), None);
        assert_eq!(which_in(OsStr::new("/bin"), "../bin/sh"), None);
        assert_eq!(which_in(OsStr::new("/bin"), ""), None);
        assert_eq!(which_in(OsStr::new("/bin"), "no-such-program-here"), None);
    }

    #[test]
    fn flags_per_shell() {
        assert_eq!(shell_args("/bin/zsh", "x"), ["-l", "-i", "-c", "x"]);
        assert_eq!(shell_args("/opt/homebrew/bin/fish", "x"), ["-l", "-i", "-c", "x"]);
        assert_eq!(shell_args("/bin/sh", "x"), ["-l", "-c", "x"]);
        assert_eq!(shell_args("/bin/tcsh", "x"), ["-c", "x"]);
    }

    #[test]
    fn dump_is_read_between_the_markers() {
        let m = "__M__";
        // NUL separated, with rc-file noise around it and a value that
        // spans lines and looks like another variable.
        let out = b"Welcome!\nPATH=/fake\n__M__<PATH=/a:/b\0X=1\nPATH=/evil\0KUBECONFIG=/k\0>__M__% \n";
        let vars = parse_dump(out, m).unwrap();
        assert_eq!(vars[0], ("PATH".to_string(), "/a:/b".to_string()));
        assert_eq!(vars[1], ("X".to_string(), "1\nPATH=/evil".to_string()));
        assert_eq!(vars[2], ("KUBECONFIG".to_string(), "/k".to_string()));
        // Newline separated (no `env -0`): the first of a name wins.
        let vars = parse_dump(b"__M__<PATH=/a\nB=x=y\nPATH=/later\n>__M__", m).unwrap();
        assert_eq!(vars, [("PATH".to_string(), "/a".to_string()), ("B".to_string(), "x=y".to_string())]);
        // Incomplete output is not used at all.
        assert!(parse_dump(b"__M__<PATH=/a\n", m).is_none());
        assert!(parse_dump(b"PATH=/a\n>__M__", m).is_none());
        assert!(parse_dump(b"", m).is_none());
    }

    #[test]
    fn dump_script_runs_in_sh() {
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", &dump_script("__T__")])
            .env("SKIFF_TEST_VALUE", "two\nlines")
            .output()
            .unwrap();
        let vars = parse_dump(&out.stdout, "__T__").unwrap();
        assert!(vars.iter().any(|(n, _)| n == "PATH"));
        // (Whole only where `env -0` exists; never split into a wrong name.)
        let v = &vars.iter().find(|(n, _)| n == "SKIFF_TEST_VALUE").unwrap().1;
        assert!(v == "two\nlines" || v == "two");
    }

    #[test]
    fn only_the_listed_variables_are_taken_and_never_over_existing_ones() {
        let shell: Vec<(String, String)> = [
            ("PATH", "/opt/bin:."),
            ("KUBECONFIG", "/home/u/.kube/a:/home/u/.kube/b"),
            ("HTTPS_PROXY", "http://proxy:3128"),
            ("http_proxy", "http://lower:3128"),
            ("NO_PROXY", "  "),
            ("AWS_PROFILE", "prod"),
            ("GITHUB_TOKEN", "secret"),
        ]
        .into_iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
        let none = |n: &str| (n == "PATH").then(|| OsString::from("/usr/bin"));
        let set = plan(Some(&shell), &none, &[]);
        let names: Vec<&str> = set.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["PATH", "KUBECONFIG", "HTTPS_PROXY", "http_proxy"]);
        assert_eq!(set[0].1, OsString::from("/opt/bin:/usr/bin"));

        // Already set here (in either spelling): left alone.
        let have = |n: &str| match n {
            "PATH" => Some(OsString::from("/opt/bin:/usr/bin")),
            "KUBECONFIG" => Some(OsString::from("/mine")),
            "https_proxy" => Some(OsString::from("http://mine")),
            "HTTP_PROXY" => Some(OsString::from("http://mine")),
            _ => None,
        };
        assert!(plan(Some(&shell), &have, &[]).is_empty());

        // No answer from the shell: only the PATH clean-up and extras.
        let rel = |n: &str| (n == "PATH").then(|| OsString::from(".:/usr/bin"));
        assert_eq!(
            plan(None, &rel, &["/usr/local/bin"]),
            [("PATH".to_string(), OsString::from("/usr/bin:/usr/local/bin"))]
        );
    }
}
