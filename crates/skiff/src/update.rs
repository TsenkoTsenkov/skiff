//! In-app updates from GitHub Releases (macOS).
//!
//! Every build the macOS workflow publishes carries a build number (the
//! workflow's run number), both compiled into the app and in the name of the
//! zipped app it attaches to the release:
//! `Skiff-macos-arm64-build-<N>.zip`. The app looks at the newest releases,
//! and when one has a higher build number than its own it offers an Update
//! button. Updating downloads the zip, unpacks it next to the installed app,
//! and on restart swaps the two and opens the new one.
//!
//! Network access goes through `curl` (always present on macOS), so the app
//! needs no HTTP or TLS code of its own for this. A public repository needs
//! no credentials; for a private one the token from `$GITHUB_TOKEN`,
//! `$GH_TOKEN` or `gh auth token` is used.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where releases are published.
pub fn repo() -> &'static str {
    option_env!("SKIFF_UPDATE_REPO").filter(|r| !r.is_empty()).unwrap_or("TsenkoTsenkov/skiff")
}

/// This build's number; 0 for a build not made by the release workflow.
pub fn build_number() -> u32 {
    option_env!("SKIFF_BUILD").and_then(|b| b.trim().parse().ok()).unwrap_or(0)
}

/// How often to look for a new release while the app is open.
const CHECK_EVERY: Duration = Duration::from_secs(4 * 3600);

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub build: u32,
    pub title: String,
    pub notes: String,
    pub page: String,
    /// API URL of the zipped app (works for private repositories too).
    pub asset_url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// Unpacked next to the installed app; applied on restart.
    Ready { release: Release, script: PathBuf },
    /// The check failed (often just offline): not worth interrupting for.
    Failed(String),
    /// Downloading or unpacking failed; offered again.
    DownloadFailed(Release, String),
}

pub struct Updater {
    state: Arc<Mutex<State>>,
    last_check: Option<Instant>,
    enabled: bool,
}

impl Default for Updater {
    fn default() -> Self {
        Updater { state: Arc::new(Mutex::new(State::Idle)), last_check: None, enabled: false }
    }
}

/// The `.app` bundle this process runs from, if any.
pub fn current_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS"))
        .then(|| bundle.to_path_buf())
}

/// Whether this copy of the app can replace itself.
pub fn can_install() -> bool {
    cfg!(target_os = "macos") && current_bundle().is_some()
}

impl Updater {
    /// Turns on checking (the real app; tests never do).
    pub fn enable(&mut self) {
        self.enabled = cfg!(target_os = "macos") && std::env::var_os("SKIFF_NO_UPDATE_CHECK").is_none();
    }

    /// Stops automatic checks (an explicit "check now" still works).
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or(State::Idle)
    }

    /// Starts a check when one is due. Call every frame; cheap.
    pub fn tick(&mut self, ctx: &egui::Context) {
        if !self.enabled {
            return;
        }
        let due = self.last_check.is_none_or(|t| t.elapsed() >= CHECK_EVERY);
        let busy = matches!(self.state(), State::Checking | State::Downloading(_) | State::Ready { .. });
        if due && !busy {
            self.check_now(ctx);
        }
    }

    pub fn check_now(&mut self, ctx: &egui::Context) {
        self.last_check = Some(Instant::now());
        self.set(State::Checking);
        let state = self.state.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let next = match latest_release() {
                Ok(Some(r)) if r.build > build_number() => State::Available(r),
                Ok(_) => State::UpToDate,
                Err(e) => State::Failed(e),
            };
            if let Ok(mut s) = state.lock() {
                *s = next;
            }
            ctx.request_repaint();
        });
    }

    /// Downloads and unpacks `release`; the state becomes `Ready` when done.
    pub fn download(&mut self, ctx: &egui::Context, release: Release) {
        self.set(State::Downloading(release.clone()));
        let state = self.state.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let next = match stage(&release) {
                Ok(script) => State::Ready { release, script },
                Err(e) => State::DownloadFailed(release, e),
            };
            if let Ok(mut s) = state.lock() {
                *s = next;
            }
            ctx.request_repaint();
        });
    }

    /// Starts the swap, which waits for this process to exit. The caller
    /// then closes the app.
    pub fn apply(&self) -> Result<(), String> {
        let State::Ready { script, .. } = self.state() else {
            return Err("no update is ready".into());
        };
        Command::new("/bin/sh")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not start the update: {e}"))
    }

    fn set(&self, next: State) {
        if let Ok(mut s) = self.state.lock() {
            *s = next;
        }
    }
}

fn token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
    }
    let out = Command::new("gh").args(["auth", "token"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let t = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

/// Runs curl; `None` token = anonymous.
fn curl(url: &str, accept: &str, token: Option<&str>, out_file: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "--max-time", "600", "-H", &format!("Accept: {accept}")]);
    cmd.args(["-H", "X-GitHub-Api-Version: 2022-11-28", "-A", "skiff-updater"]);
    // The token goes in through stdin (`-H @-`), not the command line,
    // where any local user could read it with `ps`.
    if token.is_some() {
        cmd.args(["-H", "@-"]);
    }
    if let Some(f) = out_file {
        cmd.arg("-o").arg(f);
    }
    cmd.arg(url);
    let out = match token {
        None => cmd.stdin(Stdio::null()).output(),
        Some(t) => cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(format!("Authorization: Bearer {t}\n").as_bytes())?;
            }
            child.wait_with_output()
        }),
    }
    .map_err(|e| format!("could not run curl: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("curl failed ({})", out.status) } else { err })
    }
}

/// Anonymous first; with a token if that is refused (a private repository).
fn fetch(url: &str, accept: &str, out_file: Option<&Path>) -> Result<Vec<u8>, String> {
    match curl(url, accept, None, out_file) {
        Ok(b) => Ok(b),
        Err(anon) => match token() {
            Some(t) => curl(url, accept, Some(&t), out_file),
            None => Err(anon),
        },
    }
}

fn latest_release() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{}/releases?per_page=30", repo());
    let body = fetch(&url, "application/vnd.github+json", None)?;
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    Ok(pick_release(&json, arch()))
}

/// The release with the highest build number that has a zipped app for
/// `arch`.
pub fn pick_release(json: &serde_json::Value, arch: &str) -> Option<Release> {
    let prefix = format!("Skiff-macos-{arch}-build-");
    let mut best: Option<Release> = None;
    for rel in json.as_array()? {
        if rel["draft"].as_bool() == Some(true) {
            continue;
        }
        for asset in rel["assets"].as_array().into_iter().flatten() {
            let name = asset["name"].as_str().unwrap_or("");
            let Some(build) = name.strip_prefix(&prefix).and_then(|r| r.strip_suffix(".zip")).and_then(|n| n.parse().ok())
            else {
                continue;
            };
            if best.as_ref().is_some_and(|b| b.build >= build) {
                continue;
            }
            best = Some(Release {
                build,
                title: rel["name"]
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .or(rel["tag_name"].as_str())
                    .unwrap_or("")
                    .to_string(),
                notes: rel["body"].as_str().unwrap_or("").to_string(),
                page: rel["html_url"].as_str().unwrap_or("").to_string(),
                asset_url: asset["url"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    best
}

/// Downloads and unpacks the release next to the running app, checks that
/// it starts, and writes the script that swaps it in. Returns the script.
fn stage(release: &Release) -> Result<PathBuf, String> {
    if !cfg!(target_os = "macos") {
        return Err("updating is only supported on macOS".into());
    }
    let bundle = current_bundle().ok_or("Skiff is not running from an app bundle; download the new version instead")?;
    let parent = bundle.parent().ok_or("the app has no folder")?;
    // Staged in the same folder, so that the swap is a rename.
    let staging = parent.join(format!(".skiff-update-{}", release.build));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("cannot write to {}: {e} (move Skiff to a folder you can write to)", parent.display()))?;
    let zip = staging.join("Skiff.zip");
    fetch(&release.asset_url, "application/octet-stream", Some(&zip))?;
    let ok = Command::new("ditto")
        .arg("-x")
        .arg("-k")
        .arg(&zip)
        .arg(&staging)
        .status()
        .map_err(|e| format!("could not run ditto: {e}"))?;
    if !ok.success() {
        return Err("the download could not be unpacked".into());
    }
    let _ = std::fs::remove_file(&zip);
    let new_app = staging.join("Skiff.app");
    // Downloaded by curl, so not quarantined; cleared anyway in case.
    let _ = Command::new("xattr").arg("-dr").arg("com.apple.quarantine").arg(&new_app).status();
    let version = Command::new(new_app.join("Contents/MacOS/Skiff"))
        .arg("--version")
        .output()
        .map_err(|e| format!("the new version does not start: {e}"))?;
    if !version.status.success() {
        return Err("the new version does not start".into());
    }
    let script = staging.join("apply.sh");
    std::fs::write(&script, swap_script(std::process::id(), &bundle, &new_app, &staging))
        .map_err(|e| format!("cannot write the update script: {e}"))?;
    Ok(script)
}

fn q(p: &Path) -> String {
    crate::external::sh_quote(&p.display().to_string())
}

/// Waits for `pid` to exit, puts `new_app` in place of `bundle` (putting the
/// old one back if that fails), and opens the result.
pub fn swap_script(pid: u32, bundle: &Path, new_app: &Path, staging: &Path) -> String {
    let (old, new, stage) = (q(bundle), q(new_app), q(staging));
    let backup = q(&staging.join("previous.app"));
    format!(
        "#!/bin/sh\n\
         mkdir -p \"$HOME/Library/Logs\"; exec >>\"$HOME/Library/Logs/Skiff-update.log\" 2>&1\n\
         date; echo \"updating {old}\"\n\
         while kill -0 {pid} 2>/dev/null; do sleep 0.2; done\n\
         if mv {old} {backup}; then\n\
         \x20 if mv {new} {old}; then rm -rf {backup}; else mv {backup} {old}; fi\n\
         fi\n\
         open {old}\n\
         rm -rf {stage}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_highest_build_for_this_arch() {
        let json = serde_json::json!([
            {"name": "Skiff 0.1.0 (build 7)", "tag_name": "build-7", "draft": false, "body": "b7",
             "html_url": "h7", "assets": [{"name": "Skiff-macos-arm64-build-7.zip", "url": "u7"},
                                          {"name": "Skiff-0.1.0-arm64.dmg", "url": "d7"}]},
            {"name": "Skiff 0.1.0 (build 9)", "tag_name": "build-9", "draft": true, "body": "",
             "html_url": "h9", "assets": [{"name": "Skiff-macos-arm64-build-9.zip", "url": "u9"}]},
            {"name": "", "tag_name": "build-8", "draft": false, "body": "",
             "html_url": "h8", "assets": [{"name": "Skiff-macos-arm64-build-8.zip", "url": "u8"},
                                          {"name": "Skiff-macos-x86_64-build-99.zip", "url": "x"}]}
        ]);
        let r = pick_release(&json, "arm64").unwrap();
        assert_eq!((r.build, r.title.as_str(), r.asset_url.as_str()), (8, "build-8", "u8"));
        assert!(pick_release(&serde_json::json!([]), "arm64").is_none());
    }

    #[test]
    fn swap_script_restores_on_failure() {
        let s = swap_script(
            42,
            Path::new("/Applications/Skiff.app"),
            Path::new("/Applications/.skiff-update-3/Skiff.app"),
            Path::new("/Applications/.skiff-update-3"),
        );
        assert!(s.contains("kill -0 42"));
        assert!(s.contains("mv /Applications/Skiff.app /Applications/.skiff-update-3/previous.app"));
        assert!(s.contains("else mv /Applications/.skiff-update-3/previous.app /Applications/Skiff.app"));
        assert!(s.contains("open /Applications/Skiff.app"));
    }
}
