#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use skiff::{AppOptions, SkiffApp};
use std::ffi::OsString;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const USAGE: &str = "\
Skiff - a fast, native Kubernetes cluster browser

USAGE:
    skiff [OPTIONS]

OPTIONS:
    -c, --context <NAME>     Open this kubeconfig context (default: last used / current)
    -n, --namespace <NS>     Start with only this namespace selected
        --kubeconfig <PATH>  Read clusters from this kubeconfig file instead of the
                             configured ones; repeat it to merge several files
        --view <RESOURCE>    Start on a resource view, e.g. pods or deployments.apps
        --light              Use the light theme
    -h, --help               Show this help
    -V, --version            Show the version

Clusters come from --kubeconfig, else from \"kubeconfigs\" in config.json, else from
$KUBECONFIG, else from ~/.kube/config (Windows: %USERPROFILE%\\.kube\\config),
exactly like kubectl.

Settings are read from config.json:
    Linux, macOS   $XDG_CONFIG_HOME/skiff/config.json or ~/.config/skiff/config.json
    Windows        %APPDATA%\\skiff\\config.json
    $SKIFF_CONFIG  overrides the path of the file

ENVIRONMENT:
    SKIFF_RENDERER=wgpu|glow  Draw with this renderer only. By default wgpu
                              (Vulkan, Metal, DirectX) is tried first, then
                              glow (OpenGL).";

/// The reverse-DNS id of the app: the Wayland app id and X11 window class,
/// which is how a Linux desktop finds the `.desktop` file (and so the icon)
/// of a window. The same id as the macOS bundle identifier.
const APP_ID: &str = "io.github.tsenkotsenkov.skiff";

/// The name eframe has always stored the app's state under. Must not
/// change, or everyone's saved state is lost.
const STORAGE_NAME: &str = "skiff";

/// The window and taskbar icon (the bundle's `.icns` is used on macOS, and
/// the `.ico` embedded by build.rs for the file itself on Windows).
#[cfg(not(target_os = "macos"))]
const ICON_PNG: &[u8] = include_bytes!("../assets/icons/skiff-256.png");

/// What the command line asks for.
#[derive(Debug)]
enum Cli {
    Run(AppOptions),
    Help,
    Version,
}

/// Parses the arguments after the program name. A value may follow its
/// option (`--context dev`) or be attached to a long one (`--context=dev`).
fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Cli, String> {
    let mut opts = AppOptions::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        // (Only file names may be something other than Unicode; no option is.)
        let text = arg.to_string_lossy().into_owned();
        let (name, attached) = match text.split_once('=') {
            Some((name, _)) if name.starts_with("--") => {
                // The value is cut from the original, so that a path keeps
                // bytes that are not Unicode.
                (name.to_owned(), Some(os_suffix(&arg, name.len() + 1)))
            }
            _ => (text.clone(), None),
        };
        let mut value = |what: &str| -> Result<OsString, String> {
            let v = match attached.clone() {
                Some(v) => v,
                None => args.next().ok_or_else(|| format!("{what} needs a value\n\n{USAGE}"))?,
            };
            if v.is_empty() {
                return Err(format!("{what} needs a value\n\n{USAGE}"));
            }
            Ok(v)
        };
        let string = |v: OsString| v.to_string_lossy().into_owned();
        match name.as_str() {
            "-c" | "--context" => opts.context = Some(string(value("--context")?)),
            "-n" | "--namespace" => opts.namespace = Some(string(value("--namespace")?)),
            "--view" => opts.view = Some(string(value("--view")?)),
            "--kubeconfig" => opts.kubeconfigs.push(PathBuf::from(value("--kubeconfig")?)),
            "--light" if attached.is_none() => opts.light = true,
            "-h" | "--help" if attached.is_none() => return Ok(Cli::Help),
            "-V" | "--version" if attached.is_none() => return Ok(Cli::Version),
            // (What the macOS launcher may add when the app is opened
            // from the Finder.)
            psn if psn.starts_with("-psn_") => {}
            _ => return Err(format!("unknown argument {text:?}\n\n{USAGE}")),
        }
    }
    Ok(Cli::Run(opts))
}

/// `arg` without its first `skip` bytes, which are ASCII (an option name
/// and `=`), for an argument that is not valid Unicode.
fn os_suffix(arg: &OsString, skip: usize) -> OsString {
    let bytes = arg.as_encoded_bytes();
    // SAFETY: the bytes come from an `OsString` and are split right after
    // an ASCII character, which is a boundary the encoding allows.
    unsafe { OsString::from_encoded_bytes_unchecked(bytes[skip.min(bytes.len())..].to_vec()) }
}

/// The line `--version` prints: `skiff 1.2.3 (build 45, 0a1b2c3, <target>)`.
/// It always starts with `skiff <version>`; the build number and commit
/// only appear when they were compiled in (release builds).
fn version_line(version: &str, build: u32, commit: Option<&str>, target: &str) -> String {
    let mut details: Vec<String> = Vec::new();
    if build != 0 {
        details.push(format!("build {build}"));
    }
    if let Some(commit) = commit.map(str::trim).filter(|c| !c.is_empty()) {
        // A full hash is shortened the way `git rev-parse --short` does.
        let is_hash = commit.len() > 7 && commit.bytes().all(|b| b.is_ascii_hexdigit());
        details.push(if is_hash { commit[..7].to_owned() } else { commit.to_owned() });
    }
    details.push(target.to_owned());
    format!("skiff {version} ({})", details.join(", "))
}

/// The renderers to try, in order. `choice` is `$SKIFF_RENDERER`: naming
/// one means that one only; anything else gets the default order, wgpu
/// first and glow (OpenGL) as the fallback. The second value is a warning
/// about a value that was not understood.
fn renderer_order(choice: Option<&str>) -> (Vec<eframe::Renderer>, Option<String>) {
    use eframe::Renderer::{Glow, Wgpu};
    match choice.map(|c| c.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") => (vec![Wgpu, Glow], None),
        Some("wgpu") => (vec![Wgpu], None),
        Some("glow" | "opengl" | "gl") => (vec![Glow], None),
        Some(other) => {
            (vec![Wgpu, Glow], Some(format!("SKIFF_RENDERER={other} is not known (use wgpu or glow); ignoring it.")))
        }
    }
}

fn renderer_name(renderer: eframe::Renderer) -> &'static str {
    match renderer {
        eframe::Renderer::Glow => "glow (OpenGL)",
        eframe::Renderer::Wgpu => "wgpu",
    }
}

/// Whether `error` says that this renderer cannot draw on this machine, so
/// that the other one is worth a try. (Anything else, such as no display to
/// open a window on, would fail the same way again.)
fn is_graphics_error(error: &eframe::Error) -> bool {
    matches!(
        error,
        eframe::Error::Wgpu(_) | eframe::Error::Glutin(_) | eframe::Error::NoGlutinConfigs(..) | eframe::Error::OpenGL(_)
    )
}

fn native_options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    let viewport = egui::ViewportBuilder::default()
        .with_title("Skiff")
        .with_app_id(APP_ID)
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([900.0, 560.0]);
    #[cfg(not(target_os = "macos"))]
    let viewport = match eframe::icon_data::from_png_bytes(ICON_PNG) {
        Ok(icon) => viewport.with_icon(icon),
        Err(_) => viewport,
    };
    eframe::NativeOptions {
        viewport,
        renderer,
        // eframe derives the folder of the saved state from the app id when
        // there is one. The app id is the reverse-DNS name now, but the
        // state stays where it has always been.
        persistence_path: eframe::storage_dir(STORAGE_NAME).map(|dir| dir.join("app.ron")),
        ..Default::default()
    }
}

/// Opens the window and runs the app until it is closed.
///
/// A machine without a usable GPU driver (a VM, a remote desktop session,
/// an old driver) may not be able to start the first renderer. eframe
/// reports that before the app itself is created and gives the event loop
/// back, so the other renderer is tried then. An error after the app was
/// created is not retried: the window has been up, and reopening it would
/// only hide what went wrong.
fn run(opts: AppOptions, order: &[eframe::Renderer]) -> Result<(), String> {
    let mut failures: Vec<String> = Vec::new();
    for (i, &renderer) in order.iter().enumerate() {
        let created = Arc::new(AtomicBool::new(false));
        let (created_in_app, opts) = (Arc::clone(&created), opts.clone());
        let result = eframe::run_native(
            STORAGE_NAME,
            native_options(renderer),
            Box::new(move |cc| {
                created_in_app.store(true, Ordering::Relaxed);
                Ok(Box::new(SkiffApp::new(cc, opts)))
            }),
        );
        let error = match result {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if created.load(Ordering::Relaxed) {
            return Err(format!("Skiff stopped because of a graphics error: {error}"));
        }
        if !is_graphics_error(&error) {
            return Err(format!("Skiff could not open its window: {error}"));
        }
        failures.push(format!("{}: {error}", renderer_name(renderer)));
        if let Some(&next) = order.get(i + 1) {
            eprintln!(
                "skiff: the {} renderer did not start ({error}); trying {}.",
                renderer_name(renderer),
                renderer_name(next)
            );
        }
    }
    let mut message = String::from("Skiff could not start: no graphics driver on this machine can draw its window.\n");
    for failure in &failures {
        message.push_str(&format!("\n  {failure}"));
    }
    message.push_str("\n\nUpdate the graphics driver");
    if order.len() == 1 {
        message.push_str(", or unset SKIFF_RENDERER so that the other renderer is tried too");
    }
    message.push('.');
    Err(message)
}

/// Prints `message` to stderr; where nobody can see stderr (a Windows app
/// not started from a terminal), shows it in a message box instead.
fn fail(message: &str, code: i32) -> ! {
    eprintln!("{message}");
    #[cfg(windows)]
    if !windows::has_console() {
        windows::message_box(message);
    }
    std::process::exit(code);
}

fn main() {
    skiff::app::mark_process_start();
    // First of all: without it nothing below can print in a release build.
    #[cfg(windows)]
    windows::attach_parent_console();
    let opts = match parse_args(std::env::args_os().skip(1)) {
        Ok(Cli::Run(opts)) => opts,
        Ok(Cli::Help) => {
            // (Errors ignored: `skiff --help | head` closes the pipe early.)
            let _ = writeln!(std::io::stdout(), "{USAGE}");
            std::process::exit(0);
        }
        Ok(Cli::Version) => {
            let line = version_line(
                env!("CARGO_PKG_VERSION"),
                skiff::update::build_number(),
                option_env!("SKIFF_COMMIT"),
                env!("SKIFF_TARGET"),
            );
            let _ = writeln!(std::io::stdout(), "{line}");
            std::process::exit(0);
        }
        Err(e) => fail(&e, 2),
    };
    // Both of these change the environment, so they come before anything
    // that could have started a thread.
    //
    // A release build has no console, so every credential plugin of a
    // kubeconfig (aws, gke-gcloud-auth-plugin, kubelogin, ...) would flash
    // a console window each time a token is refreshed. kube-client starts
    // them without one when this is set.
    #[cfg(windows)]
    if std::env::var_os("KUBE_RS_UNSTABLE_CREATE_NO_WINDOW").is_none() {
        std::env::set_var("KUBE_RS_UNSTABLE_CREATE_NO_WINDOW", "1");
    }
    // An app started from the Dock or a desktop launcher does not have the
    // shell's PATH, where those plugins usually are.
    #[cfg(unix)]
    skiff::shell_env::adopt_shell_path();

    let choice = std::env::var("SKIFF_RENDERER").ok();
    let (order, warning) = renderer_order(choice.as_deref());
    if let Some(warning) = warning {
        eprintln!("skiff: {warning}");
    }
    if let Err(message) = run(opts, &order) {
        fail(&message, 1);
    }
}

/// The two things a Windows app without a console needs to talk to the
/// user: the console of the terminal it was started from, and a message
/// box. Declared here rather than pulling in a bindings crate for four
/// functions.
#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::IntoRawHandle as _;

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(std_handle: u32) -> *mut c_void;
        fn SetStdHandle(std_handle: u32, handle: *mut c_void) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(window: *mut c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const MB_ICONERROR: u32 = 0x10;

    fn is_open(std_handle: u32) -> bool {
        // SAFETY: a plain query; any value is a valid argument.
        let handle = unsafe { GetStdHandle(std_handle) };
        // Null: the process has no such handle. -1: INVALID_HANDLE_VALUE.
        !handle.is_null() && handle as isize != -1
    }

    /// A release build is a GUI program (see the first line of this file),
    /// which starts without a console: `--help`, `--version` and argument
    /// errors would print nowhere. When the parent process has a console
    /// (the app was started from a terminal), this attaches to it and
    /// points stdout and stderr at it. Output that was redirected to a file
    /// or a pipe keeps going there. Does nothing when started from
    /// Explorer, or in a debug build, which has its own console.
    pub fn attach_parent_console() {
        // SAFETY: no pointers involved; fails harmlessly when there is no
        // parent console or this process already has one.
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
            return;
        }
        for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if is_open(std_handle) {
                continue;
            }
            // The console's screen buffer. The handle is given to the
            // process for the rest of its life (never closed).
            if let Ok(console) = std::fs::OpenOptions::new().read(true).write(true).open("CONOUT$") {
                // SAFETY: the handle is valid and owned by nobody else.
                unsafe { SetStdHandle(std_handle, console.into_raw_handle().cast()) };
            }
        }
    }

    /// Whether what is written to stderr goes anywhere.
    pub fn has_console() -> bool {
        is_open(STD_ERROR_HANDLE)
    }

    /// Shows `text` in an error box titled "Skiff" and waits for OK.
    pub fn message_box(text: &str) {
        let wide = |s: &str| -> Vec<u16> { std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect() };
        let (text, caption) = (wide(text), wide("Skiff"));
        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_ICONERROR) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        parse_args(args.iter().map(OsString::from))
    }

    fn run_opts(args: &[&str]) -> AppOptions {
        match parse(args) {
            Ok(Cli::Run(opts)) => opts,
            other => panic!("expected options, got {other:?}"),
        }
    }

    #[test]
    fn no_arguments_are_the_defaults() {
        let opts = run_opts(&[]);
        assert!(opts.context.is_none() && opts.namespace.is_none() && opts.view.is_none());
        assert!(!opts.light && opts.kubeconfigs.is_empty());
    }

    #[test]
    fn options_with_separate_and_attached_values() {
        let opts = run_opts(&["-c", "dev", "--namespace=kube-system", "--view", "pods", "--light"]);
        assert_eq!(opts.context.as_deref(), Some("dev"));
        assert_eq!(opts.namespace.as_deref(), Some("kube-system"));
        assert_eq!(opts.view.as_deref(), Some("pods"));
        assert!(opts.light);
    }

    #[test]
    fn kubeconfig_is_repeatable_and_keeps_its_order() {
        let opts = run_opts(&["--kubeconfig", "/a/one", "--kubeconfig=b=two.yaml", "--kubeconfig", "C:\\k\\three"]);
        assert_eq!(opts.kubeconfigs, [PathBuf::from("/a/one"), PathBuf::from("b=two.yaml"), PathBuf::from("C:\\k\\three")]);
    }

    #[cfg(unix)]
    #[test]
    fn kubeconfig_path_need_not_be_unicode() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
        let odd = OsString::from_vec(b"--kubeconfig=/tmp/k\xFFc".to_vec());
        let Ok(Cli::Run(opts)) = parse_args([odd, "--kubeconfig".into(), OsString::from_vec(b"/x\xFE".to_vec())]) else {
            panic!("expected options");
        };
        assert_eq!(opts.kubeconfigs[0].as_os_str().as_bytes(), b"/tmp/k\xFFc");
        assert_eq!(opts.kubeconfigs[1].as_os_str().as_bytes(), b"/x\xFE");
    }

    #[test]
    fn missing_and_empty_values_are_errors() {
        for args in [&["--kubeconfig"][..], &["--kubeconfig="], &["-c"], &["--view", ""]] {
            let e = parse(args).expect_err("should fail");
            assert!(e.contains("needs a value"), "{e}");
            assert!(e.contains("USAGE:"), "{e}");
        }
    }

    #[test]
    fn unknown_arguments_are_errors() {
        for args in [&["--frobnicate"][..], &["pods"], &["--light=yes"], &["--help=1"]] {
            let e = parse(args).expect_err("should fail");
            assert!(e.starts_with("unknown argument"), "{e}");
        }
    }

    #[test]
    fn help_and_version_win_over_what_follows() {
        assert!(matches!(parse(&["-h", "--frobnicate"]), Ok(Cli::Help)));
        assert!(matches!(parse(&["--help"]), Ok(Cli::Help)));
        assert!(matches!(parse(&["-V"]), Ok(Cli::Version)));
        assert!(matches!(parse(&["--version"]), Ok(Cli::Version)));
    }

    #[test]
    fn finder_process_serial_number_is_ignored() {
        assert!(run_opts(&["-psn_0_12345"]).context.is_none());
    }

    #[test]
    fn usage_documents_kubeconfig_and_config_locations() {
        for needle in ["--kubeconfig <PATH>", "config.json", "%APPDATA%\\skiff\\config.json", "%USERPROFILE%\\.kube\\config", "SKIFF_CONFIG", "SKIFF_RENDERER"] {
            assert!(USAGE.contains(needle), "usage lacks {needle}");
        }
        // Fits a terminal.
        assert!(USAGE.lines().all(|l| l.chars().count() <= 90));
    }

    #[test]
    fn version_line_formats() {
        assert_eq!(version_line("1.2.3", 0, None, "x86_64-unknown-linux-gnu"), "skiff 1.2.3 (x86_64-unknown-linux-gnu)");
        assert_eq!(
            version_line("1.2.3", 45, Some("0a1b2c3d4e5f60718293a4b5c6d7e8f901234567\n"), "aarch64-apple-darwin"),
            "skiff 1.2.3 (build 45, 0a1b2c3, aarch64-apple-darwin)"
        );
        assert_eq!(version_line("1.2.3", 0, Some("0a1b2c3"), "t"), "skiff 1.2.3 (0a1b2c3, t)");
        assert_eq!(version_line("1.2.3", 7, Some("  "), "t"), "skiff 1.2.3 (build 7, t)");
        // Not a hash: printed as given.
        assert_eq!(version_line("1.2.3", 0, Some("local-änderung"), "t"), "skiff 1.2.3 (local-änderung, t)");
    }

    #[test]
    fn renderer_order_from_the_environment() {
        use eframe::Renderer::{Glow, Wgpu};
        assert_eq!(renderer_order(None), (vec![Wgpu, Glow], None));
        assert_eq!(renderer_order(Some("")), (vec![Wgpu, Glow], None));
        assert_eq!(renderer_order(Some("wgpu")), (vec![Wgpu], None));
        assert_eq!(renderer_order(Some(" Glow ")), (vec![Glow], None));
        assert_eq!(renderer_order(Some("opengl")), (vec![Glow], None));
        let (order, warning) = renderer_order(Some("vulkan"));
        assert_eq!(order, vec![Wgpu, Glow]);
        assert!(warning.is_some_and(|w| w.contains("vulkan")));
    }

    #[test]
    fn the_target_triple_is_compiled_in() {
        assert!(env!("SKIFF_TARGET").contains('-'));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_window_icon_decodes() {
        let icon = eframe::icon_data::from_png_bytes(ICON_PNG).expect("valid PNG");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }

    #[test]
    fn saved_state_stays_under_the_old_name() {
        assert_eq!(STORAGE_NAME, "skiff");
        let options = native_options(eframe::Renderer::Wgpu);
        assert_eq!(options.viewport.app_id.as_deref(), Some(APP_ID));
        assert_eq!(options.persistence_path, eframe::storage_dir("skiff").map(|d| d.join("app.ron")));
    }
}
