//! The environment of an app that was not started from a terminal.
//!
//! On macOS an app opened from the Finder or the Dock gets a minimal `PATH`,
//! not the one the user's shell sets up. The credential helpers a kubeconfig
//! calls (`aws`, `gke-gcloud-auth-plugin`, `kubelogin`, ...) are usually
//! only on the shell's, so without it every such cluster fails to connect.

/// `PATH` made of the shell's entries first, then the current ones, then
/// `extra`, without duplicates and in that order.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn merged_path(current: &str, from_shell: Option<&str>, extra: &[&str]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let all = from_shell.unwrap_or("").split(':').chain(current.split(':')).chain(extra.iter().copied());
    for part in all {
        let part = part.trim();
        if !part.is_empty() && !parts.contains(&part) {
            parts.push(part);
        }
    }
    parts.join(":")
}

/// Gives this process the `PATH` of the user's login shell. Call it first
/// thing in `main`, before any thread exists.
#[cfg(target_os = "macos")]
pub fn adopt_shell_path() {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let current = std::env::var("PATH").unwrap_or_default();
    // Started from a terminal, the PATH already is the shell's.
    let in_terminal = std::env::var_os("TERM").is_some() || std::env::var_os("TERM_PROGRAM").is_some();
    let from_shell = if in_terminal {
        None
    } else {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".to_string());
        Command::new(shell)
            .args(["-l", "-c", "printf '%s' \"$PATH\""])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()
            .and_then(|mut child| {
                // A login shell that takes longer than this is not worth
                // delaying the window for.
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            return None;
                        }
                    }
                }
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                Some(out)
            })
    };
    // Where Homebrew puts things, in case the shell did not say.
    let extra: Vec<&str> =
        ["/opt/homebrew/bin", "/usr/local/bin"].into_iter().filter(|p| std::path::Path::new(p).is_dir()).collect();
    let merged = merged_path(&current, from_shell.as_deref(), &extra);
    if merged != current {
        // SAFETY: called at the very start of `main`; no other thread exists
        // that could be reading the environment.
        unsafe { std::env::set_var("PATH", merged) };
    }
}

#[cfg(test)]
mod tests {
    use super::merged_path;

    #[test]
    fn shell_entries_come_first_and_nothing_repeats() {
        assert_eq!(
            merged_path("/usr/bin:/bin", Some("/opt/homebrew/bin:/usr/bin"), &["/usr/local/bin", "/opt/homebrew/bin"]),
            "/opt/homebrew/bin:/usr/bin:/bin:/usr/local/bin"
        );
        assert_eq!(merged_path("/usr/bin:/bin", None, &[]), "/usr/bin:/bin");
        assert_eq!(merged_path("", Some("/a::/b\n"), &[]), "/a:/b");
    }
}
