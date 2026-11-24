//! Update checks from GitHub Releases, and in-app updating on macOS.
//!
//! # Channels, and what counts as newer
//!
//! There are two kinds of release:
//!
//! - **stable**: a normal release tagged `vX.Y.Z`;
//! - **nightly**: a prerelease tagged `build-N`, published for every push
//!   to the main branch. `N` is the build number, which only ever grows
//!   and is compiled into the app (`SKIFF_BUILD`; 0 for a build that did
//!   not come from the release workflow).
//!
//! Every release, stable ones too, carries the zipped macOS app under the
//! names `Skiff-macos-arm64-build-<N>.zip` and
//! `Skiff-macos-x86_64-build-<N>.zip`. Those names are a contract with the
//! copies already installed, which look for exactly them; they also tell
//! the build number of a stable release, on every platform.
//!
//! On the **stable** channel (the default) drafts, prereleases and tags
//! that are not exactly `vX.Y.Z` are ignored. A release is newer when
//! `X.Y.Z` is greater than this app's version (`CARGO_PKG_VERSION`), or
//! equal to it while its build number is greater than this app's and both
//! are known (so a nightly of 1.2.0 moves to the final 1.2.0, never back).
//!
//! On the **nightly** channel only drafts are ignored. A release is newer
//! when its build number is greater than this app's; where either is not
//! known (a build not made by the release workflow, a stable release
//! without the zips), when its `X.Y.Z` is greater than this app's version.
//!
//! [`is_newer`] is that rule and [`pick_release`] applies it. It is applied
//! a second time to the download itself ([`check_staged`]): the version
//! and build number in the app's signed `Info.plist` must be the ones the
//! release announced, and newer than this app's by the same rule. A release
//! that hands out an older, genuinely signed Skiff is not installed.
//!
//! # What happens with a newer release
//!
//! The check runs on every platform. Only macOS can replace the app
//! itself ([`can_install`]); elsewhere the release page is opened.
//!
//! On macOS the zip is downloaded and unpacked in a private folder next to
//! the installed app; it must hold `Skiff.app` and nothing else. Its code
//! signature is verified before anything
//! in it is run and again immediately before the swap. A build made with
//! an Apple Team ID (`SKIFF_TEAM_ID`) only accepts an app signed with a
//! Developer ID of that team and Skiff's bundle identifier; such a build
//! also leaves the quarantine mark alone. On restart a small fixed script
//! swaps the two apps and opens the new one.
//!
//! # Network
//!
//! Requests go through the system's `curl`, so the app needs no HTTP code
//! of its own for this, and only to `https://api.github.com/`. No
//! credentials are ever sent for the default repository. Only a build made
//! for another repository (`SKIFF_UPDATE_REPO`), and only after GitHub
//! refused the anonymous request with 401, 403 or 404, retries with the
//! token from `$GITHUB_TOKEN`, `$GH_TOKEN` or `gh auth token`; the token is
//! given to curl on its standard input, never on its command line.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where official releases are published.
pub const DEFAULT_REPO: &str = "TsenkoTsenkov/skiff";

/// The bundle identifier a genuine Skiff.app is signed with.
pub const BUNDLE_ID: &str = "io.github.tsenkotsenkov.skiff";

/// The only place update requests go to.
const API: &str = "https://api.github.com/";

/// Where releases are published.
pub fn repo() -> &'static str {
    option_env!("SKIFF_UPDATE_REPO").filter(|r| !r.is_empty()).unwrap_or(DEFAULT_REPO)
}

/// This build's number; 0 for a build not made by the release workflow.
pub fn build_number() -> u32 {
    option_env!("SKIFF_BUILD").and_then(|b| b.trim().parse().ok()).unwrap_or(0)
}

/// The Apple Team ID this build was signed with; `None` for an unsigned
/// (ad hoc) build.
pub fn team_id() -> Option<&'static str> {
    option_env!("SKIFF_TEAM_ID").map(str::trim).filter(|t| !t.is_empty())
}

/// This app's version.
pub fn current_version() -> Version {
    Version::parse_lenient(env!("CARGO_PKG_VERSION")).unwrap_or(Version { major: 0, minor: 0, patch: 0 })
}

/// How often to look for a new release while the app is open.
const CHECK_EVERY: Duration = Duration::from_secs(4 * 3600);

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
}

/// A version `X.Y.Z`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// A stable release tag: exactly `vX.Y.Z`, three plain numbers.
    /// `v1.2`, `1.2.3`, `v1.2.3-rc1` and `v01.2.3` are not.
    pub fn parse_tag(tag: &str) -> Option<Version> {
        Self::parse_numbers(tag.strip_prefix('v')?)
    }

    /// This app's own version, which may carry a suffix (`1.2.3-dev`).
    pub fn parse_lenient(version: &str) -> Option<Version> {
        let core = version.trim().trim_start_matches('v');
        Self::parse_numbers(core.split(['-', '+']).next()?)
    }

    fn parse_numbers(s: &str) -> Option<Version> {
        let mut parts = s.split('.');
        let mut next = || {
            let p = parts.next()?;
            let plain = !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit());
            (plain && (p == "0" || !p.starts_with('0'))).then(|| p.parse().ok())?
        };
        let v = Version { major: next()?, minor: next()?, patch: next()? };
        parts.next().is_none().then_some(v)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Which releases are followed (see the module text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Channel {
    #[default]
    Stable,
    Nightly,
}

impl Channel {
    /// The config file's `updates.channel`: `"stable"` or `"nightly"`.
    pub fn from_config(name: &str) -> Option<Channel> {
        match name.trim().to_ascii_lowercase().as_str() {
            "stable" => Some(Channel::Stable),
            "nightly" => Some(Channel::Nightly),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// The build number; 0 when the release does not tell.
    pub build: u32,
    /// `X.Y.Z` of a stable release (`vX.Y.Z` tag); `None` for a nightly.
    pub version: Option<Version>,
    pub prerelease: bool,
    pub title: String,
    pub notes: String,
    /// The release's page on github.com.
    pub page: String,
    /// API URL of the zipped macOS app for this machine; empty when the
    /// release has none (always, off macOS).
    pub asset_url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// Unpacked and verified next to the installed app (`staged` is the
    /// new app, `<staging folder>/unpacked/Skiff.app`); swapped in on
    /// restart.
    Ready {
        release: Release,
        staged: PathBuf,
    },
    /// The check failed (often just offline): not worth interrupting for.
    Failed(String),
    /// Downloading, unpacking or verifying failed; offered again.
    DownloadFailed(Release, String),
}

pub struct Updater {
    state: Arc<Mutex<State>>,
    last_check: Option<Instant>,
    /// Automatic checks are running.
    enabled: bool,
    /// `enable` was called (the real app; tests never do).
    armed: bool,
    /// The config file allows automatic checks.
    wanted: bool,
    channel: Channel,
}

impl Default for Updater {
    fn default() -> Self {
        Updater {
            state: Arc::new(Mutex::new(State::Idle)),
            last_check: None,
            enabled: false,
            armed: false,
            wanted: true,
            channel: Channel::Stable,
        }
    }
}

/// The `.app` bundle this process runs from, if any.
pub fn current_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS"))
        .then(|| bundle.to_path_buf())
}

/// Whether this copy of the app can replace itself: on macOS, running
/// from an app bundle. Everywhere else an available update means "open
/// the release page" ([`Release::page`]).
///
/// A particular update may still not be installable ([`install_blocker`]
/// says why; [`Updater::download`] then ends in `DownloadFailed` with the
/// same words).
pub fn can_install() -> bool {
    cfg!(target_os = "macos") && current_bundle().is_some()
}

/// Why `release` cannot be installed by the app itself, in words for the
/// user; `None` when it can.
pub fn install_blocker(release: &Release) -> Option<String> {
    blocker(cfg!(target_os = "macos"), current_bundle().as_deref(), &release.asset_url)
}

fn blocker(macos: bool, bundle: Option<&Path>, asset_url: &str) -> Option<String> {
    if !macos {
        return Some(
            "Skiff does not update itself on this system. Download the new version from the release page.".into(),
        );
    }
    let Some(bundle) = bundle else {
        return Some("Skiff is not running from an app bundle. Download the new version from the release page.".into());
    };
    // Opened straight from a download, macOS runs the app from a random
    // read-only place: there is nothing there to replace.
    if bundle.to_string_lossy().contains("/AppTranslocation/") {
        return Some("Move Skiff to Applications to enable updates".into());
    }
    if !is_api_url(asset_url) {
        return Some("This release has no download for this Mac. Get it from the release page.".into());
    }
    None
}

impl Updater {
    /// Applies the config file's `updates` section: whether to check
    /// automatically, and which channel to follow. May be called at any
    /// time, before or after [`Updater::enable`].
    pub fn configure(&mut self, check: bool, channel: Channel) {
        self.wanted = check;
        self.refresh_enabled();
        if channel != self.channel {
            self.channel = channel;
            // What was found for the other channel no longer holds.
            self.last_check = None;
            if matches!(self.state(), State::UpToDate | State::Available(_) | State::Failed(_)) {
                self.set(State::Idle);
            }
        }
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// Turns on automatic checking (the real app; tests never do), unless
    /// the config file or `SKIFF_NO_UPDATE_CHECK` says no. On every
    /// platform.
    pub fn enable(&mut self) {
        self.armed = true;
        self.refresh_enabled();
    }

    /// Whether automatic checks are running.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn refresh_enabled(&mut self) {
        self.enabled = self.armed && self.wanted && std::env::var_os("SKIFF_NO_UPDATE_CHECK").is_none();
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

    /// Checks now. This is what "Check for updates" does, so it runs even
    /// when automatic checks are off.
    pub fn check_now(&mut self, ctx: &egui::Context) {
        if matches!(self.state(), State::Checking | State::Downloading(_) | State::Ready { .. }) {
            return;
        }
        self.last_check = Some(Instant::now());
        self.set(State::Checking);
        let state = self.state.clone();
        let ctx = ctx.clone();
        let channel = self.channel;
        std::thread::spawn(move || {
            let next = match latest_release(channel) {
                Ok(Some(r)) => State::Available(r),
                Ok(None) => State::UpToDate,
                Err(e) => State::Failed(e),
            };
            if let Ok(mut s) = state.lock() {
                *s = next;
            }
            ctx.request_repaint();
        });
    }

    /// Downloads, unpacks and verifies `release`; the state becomes
    /// `Ready` when done, `DownloadFailed` (with the reason) otherwise.
    /// Call it when [`can_install`] says yes; otherwise open the page.
    pub fn download(&mut self, ctx: &egui::Context, release: Release) {
        if let Some(why) = install_blocker(&release) {
            self.set(State::DownloadFailed(release, why));
            return;
        }
        self.set(State::Downloading(release.clone()));
        let state = self.state.clone();
        let ctx = ctx.clone();
        let channel = self.channel;
        std::thread::spawn(move || {
            let next = match stage(&release, channel) {
                Ok(staged) => State::Ready { release, staged },
                Err(e) => State::DownloadFailed(release, e),
            };
            if let Ok(mut s) = state.lock() {
                *s = next;
            }
            ctx.request_repaint();
        });
    }

    /// Verifies the staged app once more and starts the swap, which waits
    /// for this process to exit. The caller then closes the app. On an
    /// error nothing was changed and the update is offered again.
    pub fn apply(&self) -> Result<(), String> {
        let State::Ready { release, staged } = self.state() else {
            return Err("no update is ready".into());
        };
        start_swap(&staged).inspect_err(|e| {
            if let Some(staging) = staging_of(&staged) {
                let _ = std::fs::remove_dir_all(staging);
            }
            self.set(State::DownloadFailed(release, e.clone()));
        })
    }

    fn set(&self, next: State) {
        if let Ok(mut s) = self.state.lock() {
            *s = next;
        }
    }
}

/// Keeps a helper program from flashing a console window on Windows,
/// where the app itself has none.
fn no_console(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Whether `url` is on GitHub's API host, over https. Release lists and
/// downloads are only ever requested from there, so that a tampered
/// release list cannot point curl (or a token) anywhere else.
pub fn is_api_url(url: &str) -> bool {
    url.len() > API.len() && url.starts_with(API) && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Whether a request GitHub answered with `status` is tried again with
/// the user's token: only in a build for another repository than the
/// official one (which is public), and only when the answer is the one a
/// private repository gives.
pub fn retry_with_token(status: u16, repo: &str) -> bool {
    repo != DEFAULT_REPO && matches!(status, 401 | 403 | 404)
}

/// The address of the release list for `channel`. Stable asks GitHub for
/// the one release it marks as latest (never a prerelease or a draft);
/// nightly reads the newest releases of any kind.
pub fn releases_url(repo: &str, channel: Channel) -> String {
    match channel {
        Channel::Stable => format!("{API}repos/{repo}/releases/latest"),
        Channel::Nightly => format!("{API}repos/{repo}/releases?per_page=30"),
    }
}

/// The user's GitHub token. Only looked for when [`retry_with_token`]
/// says so.
fn token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
    }
    let gh = crate::shell_env::which(if cfg!(windows) { "gh.exe" } else { "gh" })?;
    let out = no_console(Command::new(gh).args(["auth", "token"]))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let t = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

/// The system's curl, by its full path.
fn curl_path() -> Result<PathBuf, String> {
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        vec![PathBuf::from(root).join("System32").join("curl.exe")]
    } else if cfg!(target_os = "macos") {
        vec!["/usr/bin/curl".into()]
    } else {
        vec!["/usr/bin/curl".into(), "/bin/curl".into(), "/usr/local/bin/curl".into()]
    };
    candidates.into_iter().find(|p| p.is_file()).ok_or_else(|| "curl is not installed".to_string())
}

/// What curl reads from its standard input (`--config -`): the request
/// headers, among them the token when there is one. Nothing secret is on
/// the command line, which other users of the machine can see.
pub fn curl_config(accept: &str, token: Option<&str>) -> Result<String, String> {
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_graphic() && c != '"' && c != '\\');
    if !plain(accept) {
        return Err("unusable request".into());
    }
    let mut config = format!(
        "header = \"Accept: {accept}\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\nuser-agent = \"skiff-updater\"\n"
    );
    if let Some(t) = token {
        // (Anything else could not be a token, and could end the quotes.)
        if !plain(t) {
            return Err("the GitHub token has unexpected characters".into());
        }
        config.push_str(&format!("header = \"Authorization: Bearer {t}\"\n"));
    }
    Ok(config)
}

/// The arguments for curl; the answer's status code is printed on a line
/// of its own after the body.
pub fn curl_args(url: &str, out_file: Option<&Path>) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        // Must come first: without it curl also reads the user's own
        // config file (~/.curlrc), whose proxy, `insecure` or `include`
        // lines would apply to these requests.
        "-q",
        "--config",
        "-",
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "600",
        // (Redirects too: a download is redirected to GitHub's storage.)
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--write-out",
        "\\n%{http_code}",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if let Some(f) = out_file {
        args.push("--output".into());
        args.push(f.into());
    }
    args.push(url.into());
    args
}

/// Splits what curl printed into the status code and the body.
pub fn split_response(stdout: &[u8]) -> Option<(u16, &[u8])> {
    let at = stdout.iter().rposition(|b| *b == b'\n')?;
    let status = std::str::from_utf8(&stdout[at + 1..]).ok()?.trim().parse().ok()?;
    Some((status, &stdout[..at]))
}

/// One request; `None` token = anonymous. Returns the status and the body
/// (empty when written to `out_file`).
fn curl(url: &str, accept: &str, token: Option<&str>, out_file: Option<&Path>) -> Result<(u16, Vec<u8>), String> {
    use std::io::Write as _;
    if !is_api_url(url) {
        return Err("refusing to contact a server that is not GitHub's".into());
    }
    let config = curl_config(accept, token)?;
    let mut child = no_console(Command::new(curl_path()?).args(curl_args(url, out_file)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // (Dropped right after: curl sees the end of its config.)
        let _ = stdin.write_all(config.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("curl failed ({})", out.status) } else { err });
    }
    let (status, body) = split_response(&out.stdout).ok_or("unexpected answer from curl")?;
    Ok((status, body.to_vec()))
}

/// Anonymous; with the user's token only as [`retry_with_token`] allows.
fn fetch(url: &str, accept: &str, out_file: Option<&Path>) -> Result<(u16, Vec<u8>), String> {
    let anonymous = curl(url, accept, None, out_file)?;
    if retry_with_token(anonymous.0, repo()) {
        if let Some(t) = token() {
            return curl(url, accept, Some(&t), out_file);
        }
    }
    Ok(anonymous)
}

fn status_error(status: u16) -> String {
    match status {
        403 | 429 => "GitHub is limiting requests from this network. Try again later.".into(),
        _ => format!("GitHub answered with an error ({status})"),
    }
}

/// The release to offer on `channel`, if there is one newer than this app.
fn latest_release(channel: Channel) -> Result<Option<Release>, String> {
    let (status, body) = fetch(&releases_url(repo(), channel), "application/vnd.github+json", None)?;
    match status {
        200 => {}
        // No stable release has been published yet.
        404 if channel == Channel::Stable => return Ok(None),
        _ => return Err(status_error(status)),
    }
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    let arch = cfg!(target_os = "macos").then(arch);
    Ok(pick_release(&json, channel, arch, current_version(), build_number()))
}

/// `N` of an updater zip's name, `Skiff-macos-<arch>-build-<N>.zip`, and
/// its `<arch>`.
pub fn zip_build(asset_name: &str) -> Option<(&str, u32)> {
    let rest = asset_name.strip_prefix("Skiff-macos-")?.strip_suffix(".zip")?;
    let (arch, build) = rest.split_once("-build-")?;
    let plain = !build.is_empty() && build.bytes().all(|b| b.is_ascii_digit());
    (plain && !arch.is_empty()).then(|| build.parse().ok().map(|n| (arch, n)))?
}

/// One entry of GitHub's release list as a [`Release`], or `None` when
/// `channel` ignores it. `arch` is this Mac's (`arm64`, `x86_64`), `None`
/// off macOS, where no download is looked for.
fn read_release(rel: &serde_json::Value, channel: Channel, arch: Option<&str>) -> Option<Release> {
    if rel["draft"].as_bool() == Some(true) {
        return None;
    }
    let prerelease = rel["prerelease"].as_bool() == Some(true);
    let tag = rel["tag_name"].as_str().unwrap_or("");
    let version = Version::parse_tag(tag);
    let tag_build = tag
        .strip_prefix("build-")
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse::<u32>().ok());
    match channel {
        // A prerelease is never stable, whatever its tag.
        Channel::Stable if prerelease || version.is_none() => return None,
        Channel::Nightly if version.is_none() && tag_build.is_none() => return None,
        _ => {}
    }
    // The zips tell the build number (on any platform), and the one for
    // this Mac is the download.
    let (mut any_build, mut asset_url, mut asset_build) = (None::<u32>, String::new(), None::<u32>);
    for asset in rel["assets"].as_array().into_iter().flatten() {
        let Some((a, n)) = zip_build(asset["name"].as_str().unwrap_or("")) else { continue };
        any_build = any_build.max(Some(n));
        let url = asset["url"].as_str().unwrap_or("");
        if Some(a) == arch && is_api_url(url) && asset_build.is_none_or(|b| n > b) {
            asset_build = Some(n);
            asset_url = url.to_string();
        }
    }
    let page = rel["html_url"].as_str().unwrap_or("");
    Some(Release {
        build: asset_build.or(tag_build).or(any_build).unwrap_or(0),
        version,
        prerelease,
        title: rel["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(tag).to_string(),
        notes: rel["body"].as_str().unwrap_or("").to_string(),
        // (Opened in the browser: only ever a page on github.com.)
        page: if page.starts_with("https://github.com/") && !page.chars().any(|c| c.is_control() || c.is_whitespace()) {
            page.to_string()
        } else {
            format!("https://github.com/{}/releases", repo())
        },
        asset_url,
    })
}

/// Whether `release` is newer than an app of `version` and `build`
/// (0 = not known), by the rules of `channel` in the module text.
pub fn is_newer(channel: Channel, release: &Release, version: Version, build: u32) -> bool {
    let builds_known = release.build != 0 && build != 0;
    match (channel, release.version) {
        (Channel::Stable, None) => false,
        (Channel::Stable, Some(v)) => v > version || (v == version && builds_known && release.build > build),
        (Channel::Nightly, _) if builds_known => release.build > build,
        (Channel::Nightly, Some(v)) => v > version,
        (Channel::Nightly, None) => false,
    }
}

/// The newest release in GitHub's answer (a list of releases, or one)
/// that `channel` follows and that is newer than an app of `version` and
/// `build`; `None` when the app is up to date. `arch` as in
/// [`zip_build`]'s names for this Mac, `None` off macOS.
pub fn pick_release(
    json: &serde_json::Value,
    channel: Channel,
    arch: Option<&str>,
    version: Version,
    build: u32,
) -> Option<Release> {
    let one = std::slice::from_ref(json);
    let list = json.as_array().map(Vec::as_slice).unwrap_or(if json.is_object() { one } else { &[] });
    list.iter()
        .filter_map(|rel| read_release(rel, channel, arch))
        .filter(|r| is_newer(channel, r, version, build))
        // Stable: the highest version, then build. Nightly: the highest
        // build, then version (for releases that do not tell a build).
        .max_by_key(|r| match channel {
            Channel::Stable => (r.version, r.build, None),
            Channel::Nightly => (None, r.build, r.version),
        })
}

/// The code requirement a genuine update satisfies for a build signed by
/// `team`: Apple's chain, a Developer ID certificate of that team, and
/// Skiff's bundle identifier.
pub fn designated_requirement(team: &str) -> Result<String, String> {
    // (Team IDs are ten letters and digits; anything else would be read
    // as part of the requirement.)
    if team.is_empty() || team.len() > 16 || !team.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("This copy of Skiff was built with an unusable Team ID, so it cannot verify updates.".into());
    }
    Ok(format!(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team}\" and identifier \"{BUNDLE_ID}\""
    ))
}

/// The requirement for this build: `None` for an unsigned one, which can
/// only check that the download is intact.
#[cfg_attr(not(unix), allow(dead_code))]
fn requirement() -> Result<Option<String>, String> {
    team_id().map(designated_requirement).transpose()
}

/// The arguments for `/usr/bin/codesign` that verify `app`, all of it and
/// strictly, against `requirement` when there is one.
pub fn codesign_args(app: &Path, requirement: Option<&str>) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["--verify", "--deep", "--strict"].into_iter().map(OsString::from).collect();
    if let Some(r) = requirement {
        // (`=` means: the requirement itself, not a file holding one.)
        args.push("-R".into());
        args.push(format!("={r}").into());
    }
    args.push(app.into());
    args
}

/// The fixed script that swaps the apps. Run as
/// `/bin/sh -c SWAP_SCRIPT skiff-update <pid> <installed app> <new app>
/// <staging folder> <requirement or empty>`: nothing is generated, and no
/// file is left around that could be changed before it runs.
///
/// It waits for `<pid>` to exit, verifies the new app one last time, puts
/// it in place of the installed one (putting the old one back if that
/// fails) and opens the result. It only ever opens an app that is whole:
/// when the old one could not be put back, nothing is opened and the old
/// one stays in the staging folder.
///
/// The old app waits in the staging folder under a name that must be free:
/// what is put back and opened when the swap fails is then the app that
/// was installed, never something that was already lying there.
///
/// Its log is `~/Library/Logs/Skiff-update.log`. The script goes on without
/// one when that cannot be written (tried in a subshell: a redirection
/// that fails on `exec` itself would end the script, and the app would
/// quit and never come back).
pub const SWAP_SCRIPT: &str = r#"PATH=/usr/bin:/bin; export PATH
codesign=/usr/bin/codesign
open=/usr/bin/open
pid=$1; old=$2; new=$3; stage=$4; req=$5
log="$HOME/Library/Logs/Skiff-update.log"
mkdir -p "${log%/*}" 2>/dev/null
if ( : >>"$log" ) 2>/dev/null; then exec >>"$log" 2>&1; fi
date; echo "updating $old"
n=0
while kill -0 "$pid" 2>/dev/null; do
  n=$((n+1))
  if [ "$n" -gt 300 ]; then
    echo "Skiff did not quit: nothing was changed"; rm -rf "$stage"; exit 1
  fi
  sleep 0.2
done
verify() {
  [ -d "$new" ] && [ ! -L "$new" ] || return 1
  if [ -n "$req" ]; then
    "$codesign" --verify --deep --strict -R "=$req" "$new"
  else
    "$codesign" --verify --deep --strict "$new"
  fi
}
if ! verify; then
  echo "the new version did not verify: nothing was changed"
  rm -rf "$stage"; "$open" "$old"; exit 1
fi
backup="$stage/previous.app"
if [ -e "$backup" ] || [ -L "$backup" ]; then
  echo "$backup already exists: nothing was changed"
  rm -rf "$stage"; "$open" "$old"; exit 1
fi
if ! mv "$old" "$backup"; then
  echo "the installed version could not be moved: nothing was changed"
  rm -rf "$stage"; "$open" "$old"; exit 1
fi
if [ -e "$old" ] || [ -L "$old" ]; then
  echo "something is still at $old after moving it away: stopping; what was moved is at $backup"
  exit 1
fi
if mv "$new" "$old"; then
  echo "updated"
  rm -rf "$stage"; "$open" "$old"; exit 0
fi
echo "the new version could not be put in place"
if mv "$backup" "$old"; then
  echo "the previous version was put back"
  rm -rf "$stage"; "$open" "$old"; exit 1
fi
echo "the previous version could not be put back: it is at $backup"
exit 1
"#;

/// The arguments for `/bin/sh` that run [`SWAP_SCRIPT`].
pub fn swap_args(pid: u32, bundle: &Path, new_app: &Path, staging: &Path, requirement: Option<&str>) -> Vec<OsString> {
    vec![
        "-c".into(),
        SWAP_SCRIPT.into(),
        "skiff-update".into(),
        pid.to_string().into(),
        bundle.into(),
        new_app.into(),
        staging.into(),
        requirement.unwrap_or("").into(),
    ]
}

/// Whether `dir` is named like a staging folder (the only folders this
/// module ever removes).
fn is_staging_name(dir: &Path) -> bool {
    dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(".skiff-update-"))
}

/// The folder inside a staging folder that the download is unpacked into.
/// The staging folder itself then only ever holds what Skiff put there.
const UNPACKED: &str = "unpacked";

/// The staging folder of a staged app, `<staging>/unpacked/Skiff.app`;
/// `None` for any other path.
fn staging_of(staged: &Path) -> Option<&Path> {
    let unpacked = staged.parent().filter(|p| p.file_name().is_some_and(|n| n == UNPACKED))?;
    unpacked.parent().filter(|p| is_staging_name(p))
}

/// The app in the folder the download was unpacked into, which must hold
/// exactly that: one real folder named `Skiff.app` (a symlink is not
/// followed). Anything else in the zip would sit next to the app unseen by
/// every later check, so such a download is refused as a whole.
#[cfg_attr(not(unix), allow(dead_code))]
fn only_the_app(unpacked: &Path) -> Result<PathBuf, String> {
    let refuse = || "The download is not a Skiff app. Nothing was installed.".to_string();
    let mut entries = std::fs::read_dir(unpacked).map_err(|_| refuse())?;
    let (Some(Ok(first)), None) = (entries.next(), entries.next()) else {
        return Err(refuse());
    };
    let app = first.path();
    let real = std::fs::symlink_metadata(&app).is_ok_and(|m| m.file_type().is_dir());
    if first.file_name() == "Skiff.app" && real {
        Ok(app)
    } else {
        Err(refuse())
    }
}

/// Whether the app that was downloaded for `release` may be installed over
/// an app of `version` and `build` (0 = not known). `staged_version` and
/// `staged_build` are `CFBundleShortVersionString` and `CFBundleVersion` of
/// its `Info.plist`, which its signature covers: unlike the release's tag
/// and file names, they cannot be changed by someone who can only publish
/// releases. They must be what the release announced, and newer than this
/// app by the rule of `channel` ([`is_newer`]).
pub fn check_staged(
    channel: Channel,
    release: &Release,
    staged_version: &str,
    staged_build: &str,
    version: Version,
    build: u32,
) -> Result<(), String> {
    let other = "The download is not the version the release announced, so it was not installed. \
                 Get Skiff from the release page instead.";
    let staged_build = staged_build.trim();
    // (0, the number of a build of someone's own making, is "not known".)
    let digits = !staged_build.is_empty() && staged_build.bytes().all(|b| b.is_ascii_digit());
    let staged_build: u32 = if digits { staged_build.parse().map_err(|_| other.to_string())? } else { 0 };
    let Some(staged_version) = Version::parse_lenient(staged_version) else {
        return Err(other.into());
    };
    if (release.build != 0 && staged_build != release.build) || release.version.is_some_and(|v| v != staged_version) {
        return Err(format!("{other} (It is {staged_version}, build {staged_build}.)"));
    }
    let staged = Release { version: Some(staged_version), build: staged_build, ..release.clone() };
    if !is_newer(channel, &staged, version, build) {
        return Err(format!(
            "The download ({staged_version}, build {staged_build}) is not newer than this copy of Skiff, \
             so it was not installed."
        ));
    }
    Ok(())
}

/// One value of the staged app's `Info.plist`. Only read after [`verify`]:
/// the signature covers the file.
#[cfg(unix)]
fn plist_value(app: &Path, key: &str) -> Result<String, String> {
    let unreadable = || "The update's version could not be read, so it was not installed.".to_string();
    let plist = app.join("Contents/Info.plist");
    if !std::fs::symlink_metadata(&plist).is_ok_and(|m| m.file_type().is_file()) {
        return Err(unreadable());
    }
    // (PlistBuddy rather than `plutil -extract … raw`, which macOS 11 does
    // not have. `Print` only reads.)
    let out = Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(format!("Print :{key}"))
        .arg(&plist)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| unreadable())?;
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !value.is_empty() {
        Ok(value)
    } else {
        Err(unreadable())
    }
}

/// Makes a new staging folder in `parent` that only this user can enter,
/// under a name nobody can know beforehand; a path that already exists is
/// never used. Folders left by earlier updates of this user are removed.
#[cfg(unix)]
fn new_staging_dir(parent: &Path, build: u32) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let dir = parent.join(format!(".skiff-update-{build}-{:016x}", crate::shell_env::random()));
    // (Not recursive: this fails when anything is at the path already,
    // a symlink included.)
    std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} already exists; not using it", dir.display()),
        _ => format!("Skiff cannot write to {}: {e}. Move Skiff to a folder you can write to.", parent.display()),
    })?;
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| format!("cannot use {}: {e}", dir.display()))?;
    if !meta.file_type().is_dir() || meta.mode() & 0o077 != 0 {
        let _ = std::fs::remove_dir(&dir);
        return Err(format!("{} is not private; not using it", dir.display()));
    }
    // Earlier leftovers: only real folders (never through a symlink) that
    // belong to this user.
    for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
        let path = entry.path();
        if path == dir || !is_staging_name(&path) {
            continue;
        }
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir() && m.uid() == meta.uid()) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    Ok(dir)
}

/// Whether `app` looks like the unpacked Skiff.app: a real folder with a
/// real program inside, no symlink standing in for either.
#[cfg_attr(not(unix), allow(dead_code))]
fn check_layout(app: &Path) -> Result<(), String> {
    let real = |p: &Path, dir: bool| {
        std::fs::symlink_metadata(p).is_ok_and(|m| if dir { m.file_type().is_dir() } else { m.file_type().is_file() })
    };
    if real(app, true) && real(&app.join("Contents"), true) && real(&app.join("Contents/MacOS/Skiff"), false) {
        Ok(())
    } else {
        Err("The download is not a Skiff app. Nothing was installed.".into())
    }
}

/// Verifies the signature of the staged app (see the module text).
/// Nothing in it has been run when this is called.
#[cfg(unix)]
fn verify(app: &Path) -> Result<(), String> {
    check_layout(app)?;
    let requirement = requirement()?;
    let out = Command::new("/usr/bin/codesign")
        .args(codesign_args(app, requirement.as_deref()))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("The update could not be verified (codesign did not run: {e}). Nothing was installed."))?;
    if out.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&out.stderr);
    let detail = detail.lines().last().unwrap_or("").trim();
    Err(if requirement.is_some() {
        format!("The update is not signed by Skiff's developer, so it was not installed. Get Skiff from the release page instead. ({detail})")
    } else {
        format!(
            "The update is damaged, so it was not installed. Try again, or get Skiff from the release page. ({detail})"
        )
    })
}

/// Downloads and unpacks the release in a private folder next to the
/// running app and verifies it. Returns the new app, ready to be swapped
/// in.
#[cfg(unix)]
fn stage(release: &Release, channel: Channel) -> Result<PathBuf, String> {
    if let Some(why) = install_blocker(release) {
        return Err(why);
    }
    let bundle = current_bundle().ok_or("Skiff is not running from an app bundle")?;
    let parent = bundle.parent().ok_or("the app has no folder")?;
    // Staged in the same folder, so that the swap is a rename.
    let staging = new_staging_dir(parent, release.build)?;
    stage_into(&staging, release, channel).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging);
    })
}

#[cfg(unix)]
fn stage_into(staging: &Path, release: &Release, channel: Channel) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let zip = staging.join("Skiff.zip");
    let (status, _) = fetch(&release.asset_url, "application/octet-stream", Some(&zip))?;
    if status != 200 {
        return Err(format!("The download failed: {}", status_error(status)));
    }
    // Unpacked into a folder of its own: whatever the zip holds, nothing
    // of it lands in the staging folder itself.
    let unpacked = staging.join(UNPACKED);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&unpacked)
        .map_err(|e| format!("The download could not be unpacked: {e}"))?;
    let ditto = Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(&zip)
        .arg(&unpacked)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("could not run ditto: {e}"))?;
    if !ditto.success() {
        return Err("The download could not be unpacked.".into());
    }
    let _ = std::fs::remove_file(&zip);
    let new_app = only_the_app(&unpacked)?;
    // Before anything in the download is run.
    verify(&new_app)?;
    // What it says of itself, under that signature: the release it was
    // announced as, and newer than this app.
    check_staged(
        channel,
        release,
        &plist_value(&new_app, "CFBundleShortVersionString")?,
        &plist_value(&new_app, "CFBundleVersion")?,
        current_version(),
        build_number(),
    )?;
    if team_id().is_none() {
        // An unsigned build cannot pass Gatekeeper anyway; curl does not
        // set the mark, so this is only in case. A signed build keeps it:
        // Gatekeeper then checks the notarization as for any download.
        let _ = Command::new("/usr/bin/xattr")
            .arg("-dr")
            .arg("com.apple.quarantine")
            .arg(&new_app)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    // Verified, so it may run: does it start on this Mac at all?
    let version = Command::new(new_app.join("Contents/MacOS/Skiff"))
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("The new version does not start: {e}"))?;
    if !version.status.success() {
        return Err("The new version does not start on this Mac.".into());
    }
    Ok(new_app)
}

#[cfg(not(unix))]
fn stage(release: &Release, _channel: Channel) -> Result<PathBuf, String> {
    Err(install_blocker(release).unwrap_or_else(|| "Skiff does not update itself on this system.".into()))
}

/// Verifies `staged` again and starts the swap script, which outlives
/// this process.
#[cfg(unix)]
fn start_swap(staged: &Path) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("Skiff does not update itself on this system.".into());
    }
    let bundle = current_bundle().ok_or("Skiff is not running from an app bundle")?;
    let staging = staging_of(staged).ok_or("no update is ready")?;
    // The download sat on disk since it was verified: once more, right
    // before the swap (and the script does it a last time after waiting).
    verify(staged)?;
    let requirement = requirement()?;
    Command::new("/bin/sh")
        .args(swap_args(std::process::id(), &bundle, staged, staging, requirement.as_deref()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("The update could not be started: {e}"))
}

#[cfg(not(unix))]
fn start_swap(_staged: &Path) -> Result<(), String> {
    Err("Skiff does not update itself on this system.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const V: fn(u64, u64, u64) -> Version = |major, minor, patch| Version { major, minor, patch };

    /// A release as GitHub lists it. `zips`: the build number of the
    /// updater zips it carries (both architectures), if any.
    fn rel(tag: &str, prerelease: bool, draft: bool, zips: Option<u32>) -> serde_json::Value {
        let mut assets = vec![json!({"name": "Skiff-macos-universal.dmg", "url": format!("{API}assets/dmg")})];
        if let Some(n) = zips {
            for arch in ["arm64", "x86_64"] {
                assets.push(json!({
                    "name": format!("Skiff-macos-{arch}-build-{n}.zip"),
                    "url": format!("{API}repos/TsenkoTsenkov/skiff/releases/assets/{arch}-{n}"),
                }));
            }
        }
        json!({
            "name": format!("Skiff {tag}"), "tag_name": tag, "draft": draft, "prerelease": prerelease,
            "body": format!("notes of {tag}"),
            "html_url": format!("https://github.com/TsenkoTsenkov/skiff/releases/tag/{tag}"),
            "assets": assets,
        })
    }

    #[test]
    fn versions() {
        assert_eq!(Version::parse_tag("v1.2.3"), Some(V(1, 2, 3)));
        assert_eq!(Version::parse_tag("v0.10.0"), Some(V(0, 10, 0)));
        for bad in
            ["1.2.3", "v1.2", "v1.2.3.4", "v1.2.3-rc1", "v1.2.x", "v01.2.3", "v1..3", "build-7", "v", "", "v-1.2.3"]
        {
            assert_eq!(Version::parse_tag(bad), None, "{bad}");
        }
        assert_eq!(Version::parse_lenient("1.2.3-dev+abc"), Some(V(1, 2, 3)));
        assert_eq!(Version::parse_lenient("0.1.0"), Some(V(0, 1, 0)));
        assert!(V(1, 10, 0) > V(1, 9, 9) && V(2, 0, 0) > V(1, 99, 99) && V(1, 0, 1) > V(1, 0, 0));
        assert_eq!(V(1, 2, 3).to_string(), "1.2.3");
        // This crate's own version is always readable.
        assert!(Version::parse_lenient(env!("CARGO_PKG_VERSION")).is_some());
        assert_eq!(Channel::from_config(" Nightly "), Some(Channel::Nightly));
        assert_eq!(Channel::from_config("stable"), Some(Channel::Stable));
        assert_eq!(Channel::from_config("beta"), None);
    }

    #[test]
    fn zip_names_are_the_contract_with_installed_copies() {
        assert_eq!(zip_build("Skiff-macos-arm64-build-31.zip"), Some(("arm64", 31)));
        assert_eq!(zip_build("Skiff-macos-x86_64-build-7.zip"), Some(("x86_64", 7)));
        for other in [
            "Skiff-macos-universal.dmg",
            "Skiff-macos-arm64-build-.zip",
            "Skiff-macos-arm64-build-7.zip.sig",
            "Skiff-macos-arm64-build-+7.zip",
            "Skiff-macos--build-7.zip",
            "Skiff-windows-x64.zip",
            "SHA256SUMS",
        ] {
            assert_eq!(zip_build(other), None, "{other}");
        }
    }

    #[test]
    fn stable_ignores_prereleases_drafts_and_other_tags() {
        let list = json!([
            rel("build-40", true, false, Some(40)),
            rel("v9.0.0", true, false, Some(39)), // a prerelease, whatever its tag
            rel("v8.0.0", false, true, Some(38)), // a draft
            rel("v7.0.0-rc1", false, false, Some(37)),
            rel("build-36", false, false, Some(36)), // an old-style release that was not marked
            rel("v1.1.0", false, false, Some(35)),
            rel("v1.0.0", false, false, Some(30)),
        ]);
        let r = pick_release(&list, Channel::Stable, Some("arm64"), V(1, 0, 0), 30).unwrap();
        assert_eq!((r.version, r.build, r.prerelease), (Some(V(1, 1, 0)), 35, false));
        assert_eq!(r.title, "Skiff v1.1.0");
        assert_eq!(r.notes, "notes of v1.1.0");
        assert_eq!(r.page, "https://github.com/TsenkoTsenkov/skiff/releases/tag/v1.1.0");
        // A stable release carries the zips; this Mac's is the download.
        assert_eq!(r.asset_url, format!("{API}repos/TsenkoTsenkov/skiff/releases/assets/arm64-35"));
        let intel = pick_release(&list, Channel::Stable, Some("x86_64"), V(1, 0, 0), 30).unwrap();
        assert!(intel.asset_url.ends_with("/x86_64-35"));
        // Up to date.
        assert_eq!(pick_release(&list, Channel::Stable, Some("arm64"), V(1, 1, 0), 35), None);
        assert_eq!(pick_release(&list, Channel::Stable, Some("arm64"), V(2, 0, 0), 0), None);
    }

    #[test]
    fn stable_compares_versions_then_builds() {
        let latest = rel("v1.2.0", false, false, Some(50));
        let pick = |version, build| pick_release(&latest, Channel::Stable, None, version, build).is_some();
        // (GitHub's "latest" answer is one release, not a list.)
        assert!(pick(V(1, 1, 9), 49));
        assert!(pick(V(1, 1, 9), 99)); // the version decides first
        assert!(pick(V(1, 1, 9), 0));
        // The same version: only a known, higher build number is newer
        // (a nightly of 1.2.0 moves to the final 1.2.0).
        assert!(pick(V(1, 2, 0), 48));
        assert!(!pick(V(1, 2, 0), 50));
        assert!(!pick(V(1, 2, 0), 51));
        assert!(!pick(V(1, 2, 0), 0)); // a build of our own making: unknown
        assert!(!pick(V(1, 3, 0), 10)); // never back to an older version
                                        // A stable release without zips: only the version can tell.
        let bare = rel("v1.2.0", false, false, None);
        assert!(pick_release(&bare, Channel::Stable, None, V(1, 1, 0), 5).is_some());
        assert!(pick_release(&bare, Channel::Stable, None, V(1, 2, 0), 5).is_none());
    }

    #[test]
    fn nightly_compares_build_numbers() {
        let list = json!([
            rel("build-42", true, false, Some(42)),
            rel("build-41", true, false, Some(41)),
            rel("v1.0.0", false, false, Some(40)),
            rel("build-99", true, true, Some(99)), // a draft
            rel("weekly", true, false, None),      // not ours
        ]);
        let r = pick_release(&list, Channel::Nightly, Some("arm64"), V(1, 0, 0), 40).unwrap();
        assert_eq!((r.build, r.version, r.prerelease), (42, None, true));
        assert!(r.asset_url.ends_with("/arm64-42"));
        assert_eq!(pick_release(&list, Channel::Nightly, Some("arm64"), V(1, 0, 0), 42), None);
        assert_eq!(pick_release(&list, Channel::Nightly, Some("arm64"), V(1, 0, 0), 43), None);
        // Stable releases count on nightly too, by their build number:
        // the version does not pull a newer nightly back.
        let stable_only = json!([rel("v1.0.1", false, false, Some(45))]);
        assert!(pick_release(&stable_only, Channel::Nightly, None, V(1, 0, 0), 44).is_some());
        assert!(pick_release(&stable_only, Channel::Nightly, None, V(1, 0, 0), 50).is_none());
        // A build of our own making has no number: only a higher version
        // is offered, never a nightly.
        let r = pick_release(&list, Channel::Nightly, None, V(0, 9, 0), 0).unwrap();
        assert_eq!(r.version, Some(V(1, 0, 0)));
        assert_eq!(pick_release(&list, Channel::Nightly, None, V(1, 0, 0), 0), None);
        // The same list on stable: the nightlies are not there at all.
        assert_eq!(pick_release(&list, Channel::Stable, Some("arm64"), V(1, 0, 0), 40), None);
    }

    #[test]
    fn windows_and_linux_need_no_download_in_the_release() {
        // No platform asset is looked for: the release page is opened.
        let list = json!([{
            "name": "", "tag_name": "v2.0.0", "draft": false, "prerelease": false, "body": "",
            "html_url": "https://github.com/TsenkoTsenkov/skiff/releases/tag/v2.0.0",
            "assets": [{"name": "Skiff-windows-x64-setup.exe", "url": "https://api.github.com/x"}],
        }]);
        let r = pick_release(&list, Channel::Stable, None, V(1, 0, 0), 12).unwrap();
        assert_eq!((r.title.as_str(), r.build, r.asset_url.as_str()), ("v2.0.0", 0, ""));
        assert!(blocker(false, None, &r.asset_url).is_some());
        // With the zips present the build number is known there as well.
        let r = pick_release(&rel("v2.0.0", false, false, Some(77)), Channel::Stable, None, V(1, 0, 0), 12).unwrap();
        assert_eq!((r.build, r.asset_url.as_str()), (77, ""));
        // On a Mac a release without this Mac's zip is still announced,
        // but cannot be installed by the app.
        let r = pick_release(&list, Channel::Stable, Some("arm64"), V(1, 0, 0), 12).unwrap();
        assert!(blocker(true, Some(Path::new("/Applications/Skiff.app")), &r.asset_url).is_some());
    }

    #[test]
    fn urls_from_the_release_list_are_not_trusted() {
        let evil = json!([{
            "name": "x", "tag_name": "v2.0.0", "draft": false, "prerelease": false, "body": "",
            "html_url": "https://evil.example/skiff",
            "assets": [{"name": "Skiff-macos-arm64-build-9.zip", "url": "https://evil.example/Skiff.zip"}],
        }]);
        let r = pick_release(&evil, Channel::Stable, Some("arm64"), V(1, 0, 0), 1).unwrap();
        assert_eq!(r.asset_url, "");
        assert_eq!(r.page, format!("https://github.com/{}/releases", repo()));
        assert!(is_api_url("https://api.github.com/repos/a/b/releases/assets/1"));
        for bad in [
            "https://api.github.com/",
            "http://api.github.com/repos",
            "https://api.github.com.evil.example/repos",
            "https://api.github.com@evil.example/",
            "https://evil.example/https://api.github.com/",
            "https://api.github.com/a b",
            "https://api.github.com/a\nb",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!is_api_url(bad), "{bad}");
        }
        // Unexpected shapes are no release, not a panic.
        for odd in
            [json!(null), json!("x"), json!([1, "a", null]), json!({"message": "Not Found"}), json!([{"assets": 3}])]
        {
            assert_eq!(pick_release(&odd, Channel::Nightly, Some("arm64"), V(0, 0, 0), 1), None);
            assert_eq!(pick_release(&odd, Channel::Stable, Some("arm64"), V(0, 0, 0), 1), None);
        }
    }

    #[test]
    fn release_lists_per_channel() {
        assert_eq!(releases_url("a/b", Channel::Stable), "https://api.github.com/repos/a/b/releases/latest");
        assert_eq!(releases_url("a/b", Channel::Nightly), "https://api.github.com/repos/a/b/releases?per_page=30");
        assert!(is_api_url(&releases_url(DEFAULT_REPO, Channel::Stable)));
    }

    #[test]
    fn the_token_is_only_for_other_repositories_that_refuse() {
        for status in [200, 401, 403, 404, 429, 500] {
            assert!(!retry_with_token(status, DEFAULT_REPO), "{status}");
        }
        for status in [401, 403, 404] {
            assert!(retry_with_token(status, "someone/private-fork"), "{status}");
        }
        for status in [200, 301, 429, 500, 0] {
            assert!(!retry_with_token(status, "someone/private-fork"), "{status}");
        }
    }

    #[test]
    fn the_token_goes_to_curl_on_stdin_not_in_its_arguments() {
        let config = curl_config("application/vnd.github+json", Some("ghp_s3cret")).unwrap();
        assert!(config.contains("header = \"Authorization: Bearer ghp_s3cret\"\n"));
        assert!(config.contains("header = \"Accept: application/vnd.github+json\"\n"));
        assert!(!curl_config("application/octet-stream", None).unwrap().contains("Authorization"));
        // Nothing that could end the quoted value or add an option.
        for bad in ["a\"b", "a\\b", "a\nurl = \"https://evil\"", "a b", ""] {
            assert!(curl_config("x/y", Some(bad)).is_err(), "{bad:?}");
        }
        let args = curl_args("https://api.github.com/x", Some(Path::new("/s/Skiff.zip")));
        // `-q` only counts as the very first argument.
        assert_eq!(args[..3], [OsString::from("-q"), OsString::from("--config"), OsString::from("-")]);
        assert_eq!(curl_args("https://api.github.com/x", None)[0], "-q");
        assert!(args.iter().all(|a| !a.to_string_lossy().contains("Bearer")));
        assert!(args.windows(2).any(|w| w[0] == "--proto-redir" && w[1] == "=https"));
        assert!(args.windows(2).any(|w| w[0] == "--output" && w[1] == "/s/Skiff.zip"));
        assert_eq!(args.last().unwrap(), "https://api.github.com/x");
    }

    #[test]
    fn curl_output_is_split_into_status_and_body() {
        assert_eq!(split_response(b"[{\"a\":1}]\n200"), Some((200, &b"[{\"a\":1}]"[..])));
        assert_eq!(split_response(b"line\nline\n\n404"), Some((404, &b"line\nline\n"[..])));
        assert_eq!(split_response(b"\n200"), Some((200, &b""[..])));
        assert_eq!(split_response(b"200"), None);
        assert_eq!(split_response(b"body\nnot a number"), None);
    }

    #[test]
    fn requirement_pins_team_and_bundle_id() {
        let r = designated_requirement("AB12CDE345").unwrap();
        assert_eq!(
            r,
            "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
             and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
             and certificate leaf[subject.OU] = \"AB12CDE345\" and identifier \"io.github.tsenkotsenkov.skiff\""
        );
        // (The identifier is the one releases are made with now, not the
        // one older copies had.)
        assert!(!r.contains("dev.skiff.Skiff"));
        for bad in ["", "AB12\" or anchor apple", "AB 12", "AB12CDE345AB12CDE345", "a/b"] {
            assert!(designated_requirement(bad).is_err(), "{bad:?}");
        }
        let app = Path::new("/Applications/.skiff-update-9-ab/Skiff.app");
        assert_eq!(
            codesign_args(app, None),
            ["--verify", "--deep", "--strict", "/Applications/.skiff-update-9-ab/Skiff.app"]
        );
        let with = codesign_args(app, Some(&r));
        assert_eq!(with[3], "-R");
        assert_eq!(with[4].to_string_lossy(), format!("={r}"));
        assert_eq!(with.len(), 6);
    }

    #[test]
    fn what_blocks_installing() {
        let url = "https://api.github.com/repos/a/b/releases/assets/1";
        assert_eq!(blocker(true, Some(Path::new("/Applications/Skiff.app")), url), None);
        let moved =
            blocker(true, Some(Path::new("/private/var/folders/x/T/AppTranslocation/1B2C-3D/d/Skiff.app")), url);
        assert_eq!(moved.as_deref(), Some("Move Skiff to Applications to enable updates"));
        assert!(blocker(true, None, url).unwrap().contains("app bundle"));
        assert!(blocker(false, None, url).unwrap().contains("release page"));
        assert!(blocker(true, Some(Path::new("/Applications/Skiff.app")), "").is_some());
    }

    #[test]
    fn settings() {
        let mut u = Updater::default();
        assert!(!u.is_enabled() && u.channel() == Channel::Stable);
        // (Tests never call `enable`; configuring alone starts nothing.)
        u.configure(true, Channel::Nightly);
        assert!(!u.is_enabled() && u.channel() == Channel::Nightly);
        u.set(State::UpToDate);
        u.configure(true, Channel::Nightly);
        assert_eq!(u.state(), State::UpToDate);
        // Another channel: what was found before no longer holds.
        u.configure(false, Channel::Stable);
        assert_eq!(u.state(), State::Idle);
        assert!(u.apply().is_err());
    }

    #[test]
    fn swap_runs_a_fixed_script_with_arguments() {
        let args = swap_args(
            42,
            Path::new("/Applications/Skiff.app"),
            Path::new("/Applications/.skiff-update-3-ff/Skiff.app"),
            Path::new("/Applications/.skiff-update-3-ff"),
            None,
        );
        assert_eq!(args[0], "-c");
        assert_eq!(args[1].to_string_lossy(), SWAP_SCRIPT);
        assert_eq!(
            args[2..],
            [
                "skiff-update",
                "42",
                "/Applications/Skiff.app",
                "/Applications/.skiff-update-3-ff/Skiff.app",
                "/Applications/.skiff-update-3-ff",
                ""
            ]
        );
        // No path, pid or name is part of the script's text.
        assert!(!SWAP_SCRIPT.contains("Applications") && !SWAP_SCRIPT.contains("42"));
        for tool in ["codesign=/usr/bin/codesign\n", "open=/usr/bin/open\n", "PATH=/usr/bin:/bin;"] {
            assert!(SWAP_SCRIPT.contains(tool));
        }
        assert!(is_staging_name(Path::new("/Applications/.skiff-update-3-ff")));
        assert!(!is_staging_name(Path::new("/Applications")));
        // The staged app is two levels inside its staging folder.
        assert_eq!(
            staging_of(Path::new("/Applications/.skiff-update-3-ff/unpacked/Skiff.app")),
            Some(Path::new("/Applications/.skiff-update-3-ff"))
        );
        for other in ["/Applications/.skiff-update-3-ff/Skiff.app", "/Applications/unpacked/Skiff.app", "/Skiff.app"] {
            assert_eq!(staging_of(Path::new(other)), None, "{other}");
        }
    }

    #[test]
    fn the_download_itself_must_be_the_newer_release() {
        let release = |tag: &str, build| {
            let r = pick_release(&rel(tag, false, false, Some(build)), Channel::Nightly, Some("arm64"), V(0, 0, 0), 1);
            r.unwrap()
        };
        // Stable 1.2.0 (build 50) over 1.1.0 (build 40).
        let stable = release("v1.2.0", 50);
        let check = |channel, r: &Release, v: &str, b: &str| check_staged(channel, r, v, b, V(1, 1, 0), 40);
        assert_eq!(check(Channel::Stable, &stable, "1.2.0", "50"), Ok(()));
        assert_eq!(check(Channel::Stable, &stable, " 1.2.0\n", "50\n"), Ok(()));
        // An older, genuinely signed app under the new release's names.
        let err = check(Channel::Stable, &stable, "1.0.0", "30").unwrap_err();
        assert!(err.contains("not the version the release announced") && err.contains("1.0.0, build 30"), "{err}");
        // Either number alone gives it away.
        assert!(check(Channel::Stable, &stable, "1.2.0", "30").is_err());
        assert!(check(Channel::Stable, &stable, "1.0.0", "50").is_err());
        // What cannot be read is not installed.
        for (v, b) in [("", "50"), ("x", "50"), ("1.2.0", ""), ("1.2.0", "fifty"), ("1.2.0", "99999999999")] {
            assert!(check(Channel::Stable, &stable, v, b).is_err(), "{v:?} {b:?}");
        }
        // A nightly tells no version: the build number in the app decides.
        let nightly = release("build-45", 45);
        assert_eq!(nightly.version, None);
        assert_eq!(check(Channel::Nightly, &nightly, "1.1.0", "45"), Ok(()));
        assert!(check(Channel::Nightly, &nightly, "1.1.0", "39").is_err());
        // The app is what was announced, but this copy is not older: the
        // same rule as for the release list.
        let same = check_staged(Channel::Nightly, &nightly, "1.1.0", "45", V(1, 1, 0), 45).unwrap_err();
        assert!(same.contains("not newer"), "{same}");
        assert!(check_staged(Channel::Nightly, &nightly, "1.1.0", "45", V(1, 1, 0), 46).is_err());
        assert!(check_staged(Channel::Stable, &stable, "1.2.0", "50", V(1, 3, 0), 10).is_err());
        assert_eq!(check_staged(Channel::Stable, &stable, "1.2.0", "50", V(1, 2, 0), 48), Ok(()));
        // A build of our own making (no number): only the version counts.
        assert_eq!(check_staged(Channel::Stable, &stable, "1.2.0", "50", V(1, 1, 0), 0), Ok(()));
        assert!(check_staged(Channel::Stable, &stable, "1.2.0", "50", V(1, 2, 0), 0).is_err());
        assert_eq!(check_staged(Channel::Nightly, &stable, "1.2.0", "50", V(1, 1, 0), 0), Ok(()));
        assert!(check_staged(Channel::Nightly, &nightly, "1.1.0", "45", V(1, 1, 0), 0).is_err());
    }

    /// The swap script itself, run by `/bin/sh` in a scratch folder with
    /// stand-ins for `codesign` and `open`.
    #[cfg(unix)]
    mod swap {
        use super::*;
        use std::os::unix::fs::PermissionsExt as _;

        struct Scene {
            root: PathBuf,
            old: PathBuf,
            new: PathBuf,
            stage: PathBuf,
        }

        fn scene() -> Scene {
            let root = std::env::temp_dir().join(format!("skiff-swap-test-{:016x}", crate::shell_env::random()));
            let (old, stage) = (root.join("Apps/Skiff.app"), root.join("Apps/.skiff-update-9-ab"));
            let new = stage.join(UNPACKED).join("Skiff.app");
            for (dir, which) in [(&old, "old"), (&new, "new")] {
                std::fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
                std::fs::write(dir.join("Contents/MacOS/Skiff"), which).unwrap();
            }
            Scene { root, old, new, stage }
        }

        /// Runs the script; `codesign` is the body of the stand-in.
        /// Returns whether it reported success and what was opened.
        fn run(s: &Scene, codesign: &str, requirement: Option<&str>) -> (bool, String) {
            let tool = |name: &str, body: &str| {
                let p = s.root.join(name);
                std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
                p
            };
            let opened = s.root.join("opened");
            let codesign =
                tool("codesign", &format!("printf '%s\\n' \"$@\" >>'{}/codesign-args'\n{codesign}", s.root.display()));
            let open = tool("open", &format!("cat \"$1/Contents/MacOS/Skiff\" >>'{}'", opened.display()));
            let script = SWAP_SCRIPT
                .replace("codesign=/usr/bin/codesign\n", &format!("codesign='{}'\n", codesign.display()))
                .replace("open=/usr/bin/open\n", &format!("open='{}'\n", open.display()));
            assert_ne!(script, SWAP_SCRIPT);
            // A process that has ended: nothing to wait for.
            let mut done = Command::new("/bin/sh").args(["-c", "exit 0"]).spawn().unwrap();
            let pid = done.id();
            done.wait().unwrap();
            let mut args = swap_args(pid, &s.old, &s.new, &s.stage, requirement);
            args[1] = script.into();
            let status = Command::new("/bin/sh").args(args).env("HOME", &s.root).status().unwrap();
            (status.success(), std::fs::read_to_string(&opened).unwrap_or_default())
        }

        fn log(s: &Scene) -> String {
            std::fs::read_to_string(s.root.join("Library/Logs/Skiff-update.log")).unwrap_or_default()
        }

        fn which(app: &Path) -> String {
            std::fs::read_to_string(app.join("Contents/MacOS/Skiff")).unwrap_or_default()
        }

        #[test]
        fn swaps_and_opens_the_new_app() {
            let s = scene();
            let (ok, opened) = run(&s, "exit 0", Some("anchor apple generic and identifier \"x\""));
            assert!(ok);
            assert_eq!((which(&s.old).as_str(), opened.as_str()), ("new", "new"));
            assert!(!s.stage.exists());
            assert!(log(&s).ends_with("updated\n"), "{}", log(&s));
            // Verified right before the swap, against the requirement.
            let args = std::fs::read_to_string(s.root.join("codesign-args")).unwrap();
            let want = format!(
                "--verify\n--deep\n--strict\n-R\n=anchor apple generic and identifier \"x\"\n{}\n",
                s.new.display()
            );
            assert_eq!(args, want);
            let _ = std::fs::remove_dir_all(&s.root);
        }

        #[test]
        fn an_app_that_does_not_verify_is_not_installed() {
            let s = scene();
            let (ok, opened) = run(&s, "exit 1", None);
            assert!(!ok);
            assert_eq!((which(&s.old).as_str(), opened.as_str()), ("old", "old"));
            assert!(!s.stage.exists());
            let args = std::fs::read_to_string(s.root.join("codesign-args")).unwrap();
            assert_eq!(args, format!("--verify\n--deep\n--strict\n{}\n", s.new.display()));
            let _ = std::fs::remove_dir_all(&s.root);
        }

        #[test]
        fn a_symlink_in_place_of_the_new_app_is_not_installed() {
            let s = scene();
            std::fs::rename(&s.new, s.root.join("elsewhere.app")).unwrap();
            std::os::unix::fs::symlink(s.root.join("elsewhere.app"), &s.new).unwrap();
            let (ok, opened) = run(&s, "exit 0", None);
            assert!(!ok);
            assert_eq!((which(&s.old).as_str(), opened.as_str()), ("old", "old"));
            // (Not even looked at by codesign.)
            assert!(!s.root.join("codesign-args").exists());
            let _ = std::fs::remove_dir_all(&s.root);
        }

        #[test]
        fn the_old_app_is_put_back_when_the_new_one_cannot_be_moved() {
            let s = scene();
            // The new app vanishes between the check and the move.
            let (ok, opened) = run(&s, "for a in \"$@\"; do last=$a; done; rm -rf \"$last\"; exit 0", None);
            assert!(!ok);
            assert_eq!((which(&s.old).as_str(), opened.as_str()), ("old", "old"));
            assert!(!s.stage.exists());
            let _ = std::fs::remove_dir_all(&s.root);
        }

        /// A folder named like the backup that was already in the staging
        /// folder (from the zip, say) holding an app nobody verified.
        fn plant_backup(s: &Scene) {
            let planted = s.stage.join("previous.app");
            std::fs::create_dir_all(planted.join("Contents/MacOS")).unwrap();
            std::fs::write(planted.join("Contents/MacOS/Skiff"), "planted").unwrap();
        }

        #[test]
        fn a_backup_name_that_is_taken_changes_nothing() {
            let s = scene();
            plant_backup(&s);
            let (ok, opened) = run(&s, "exit 0", None);
            assert!(!ok);
            assert_eq!((which(&s.old).as_str(), opened.as_str()), ("old", "old"));
            assert!(!s.stage.exists());
            assert!(log(&s).contains("already exists"), "{}", log(&s));
            // A symlink there, even one that leads nowhere, as well.
            let s2 = scene();
            std::os::unix::fs::symlink(s2.root.join("nowhere"), s2.stage.join("previous.app")).unwrap();
            let (ok, opened) = run(&s2, "exit 0", None);
            assert!(!ok);
            assert_eq!((which(&s2.old).as_str(), opened.as_str()), ("old", "old"));
            for s in [s, s2] {
                let _ = std::fs::remove_dir_all(&s.root);
            }
        }

        #[test]
        fn what_is_put_back_is_the_installed_app_not_a_planted_one() {
            let s = scene();
            plant_backup(&s);
            // The move of the new app fails too (it vanishes): what is at
            // the installed path and opened must be the original.
            let (ok, opened) = run(&s, "for a in \"$@\"; do last=$a; done; rm -rf \"$last\"; exit 0", None);
            assert!(!ok);
            assert_eq!((which(&s.old).as_str(), opened.as_str()), ("old", "old"));
            assert!(!s.old.join("Skiff.app").exists());
            let _ = std::fs::remove_dir_all(&s.root);
        }

        #[test]
        fn a_log_that_cannot_be_written_does_not_stop_the_swap() {
            let s = scene();
            // A folder where the log file should be.
            std::fs::create_dir_all(s.root.join("Library/Logs/Skiff-update.log")).unwrap();
            let (ok, opened) = run(&s, "exit 0", None);
            assert!(ok);
            assert_eq!((which(&s.old).as_str(), opened.as_str()), ("new", "new"));
            assert!(!s.stage.exists());
            // The Logs folder itself cannot be made: a file is in the way.
            let s2 = scene();
            std::fs::write(s2.root.join("Library"), "").unwrap();
            let (ok, opened) = run(&s2, "exit 0", None);
            assert!(ok);
            assert_eq!((which(&s2.old).as_str(), opened.as_str()), ("new", "new"));
            for s in [s, s2] {
                let _ = std::fs::remove_dir_all(&s.root);
            }
        }

        #[test]
        fn the_zip_may_hold_the_app_and_nothing_else() {
            let s = scene();
            let unpacked = s.stage.join(UNPACKED);
            assert_eq!(only_the_app(&unpacked), Ok(s.new.clone()));
            assert_eq!(staging_of(&s.new), Some(s.stage.as_path()));
            // Anything next to it: a folder, a file, a symlink.
            let extra = unpacked.join("previous.app");
            std::fs::create_dir(&extra).unwrap();
            assert!(only_the_app(&unpacked).is_err());
            std::fs::remove_dir(&extra).unwrap();
            std::fs::write(unpacked.join(".hidden"), "").unwrap();
            assert!(only_the_app(&unpacked).is_err());
            std::fs::remove_file(unpacked.join(".hidden")).unwrap();
            std::os::unix::fs::symlink("/tmp", &extra).unwrap();
            assert!(only_the_app(&unpacked).is_err());
            std::fs::remove_file(&extra).unwrap();
            assert!(only_the_app(&unpacked).is_ok());
            // The app under another name, a symlink named like it, a file
            // named like it, nothing at all, no folder at all.
            std::fs::rename(&s.new, unpacked.join("Other.app")).unwrap();
            assert!(only_the_app(&unpacked).is_err());
            std::os::unix::fs::symlink(unpacked.join("Other.app"), &s.new).unwrap();
            assert!(only_the_app(&unpacked).is_err());
            std::fs::remove_dir_all(unpacked.join("Other.app")).unwrap();
            assert!(only_the_app(&unpacked).is_err());
            std::fs::remove_file(&s.new).unwrap();
            assert!(only_the_app(&unpacked).is_err());
            std::fs::write(&s.new, "").unwrap();
            assert!(only_the_app(&unpacked).is_err());
            assert!(only_the_app(&s.root.join("nothing")).is_err());
            let _ = std::fs::remove_dir_all(&s.root);
        }

        #[test]
        fn a_missing_installed_app_changes_nothing() {
            let s = scene();
            std::fs::remove_dir_all(&s.old).unwrap();
            let (ok, opened) = run(&s, "exit 0", None);
            assert!(!ok);
            // The new app was not put where nothing could be moved away.
            assert_eq!(opened, "");
            assert!(!s.old.exists() && !s.stage.exists());
            let _ = std::fs::remove_dir_all(&s.root);
        }

        #[test]
        fn staging_folders_are_private_and_fresh() {
            use std::os::unix::fs::MetadataExt as _;
            let root = std::env::temp_dir().join(format!("skiff-stage-test-{:016x}", crate::shell_env::random()));
            std::fs::create_dir_all(&root).unwrap();
            // Leftovers: a folder of ours, and a symlink someone planted.
            let leftover = root.join(".skiff-update-1-old");
            std::fs::create_dir_all(leftover.join("Skiff.app")).unwrap();
            let target = root.join("precious");
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("file"), "keep").unwrap();
            std::os::unix::fs::symlink(&target, root.join(".skiff-update-2-link")).unwrap();

            let a = new_staging_dir(&root, 7).unwrap();
            let b = new_staging_dir(&root, 7).unwrap();
            assert_ne!(a, b);
            assert!(is_staging_name(&b) && b.parent() == Some(root.as_path()));
            let meta = std::fs::symlink_metadata(&b).unwrap();
            assert!(meta.file_type().is_dir());
            assert_eq!(meta.mode() & 0o777, 0o700);
            // The earlier ones of ours are gone; the symlink was not followed.
            assert!(!leftover.exists() && !a.exists());
            assert_eq!(std::fs::read_to_string(target.join("file")).unwrap(), "keep");
            // A folder that cannot be written to is said so.
            let err = new_staging_dir(&root.join("missing"), 7).unwrap_err();
            assert!(err.contains("cannot write to"), "{err}");
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn the_unpacked_app_must_be_real() {
            let s = scene();
            assert!(check_layout(&s.new).is_ok());
            assert!(check_layout(&s.stage).is_err());
            assert!(check_layout(&s.root.join("nothing")).is_err());
            let link = s.stage.join("Link.app");
            std::os::unix::fs::symlink(&s.new, &link).unwrap();
            assert!(check_layout(&link).is_err());
            // The program itself replaced by a symlink.
            let exe = s.new.join("Contents/MacOS/Skiff");
            std::fs::remove_file(&exe).unwrap();
            std::os::unix::fs::symlink("/bin/sh", &exe).unwrap();
            assert!(check_layout(&s.new).is_err());
            let _ = std::fs::remove_dir_all(&s.root);
        }
    }
}
