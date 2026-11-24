//! Records the frames of the demo animation shown at the top of the README
//! and on the site: the real app, rendered headlessly, driven through a
//! scripted tour of the `demo` test cluster.
//!
//! Opt-in (`#[ignore]`), because it is a recording and not a check:
//!
//!   hack/cluster-up.sh
//!   hack/demo/render.sh        # records, then renders the GIF and the MP4
//!
//! or, for the frames alone:
//!
//!   cargo test -p skiff --test demo -- --ignored --nocapture
//!
//! The frames land in `target/demo/frames/` as numbered PNGs, next to
//! `manifest.jsonl`: one line per frame with how long it stays on screen
//! and where the pointer is, and one line per caption. The app draws
//! everything in the frames; `hack/demo/render.py` adds the caption bar,
//! the pointer, the click rings and the cross-fades between scenes.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use skiff::{AppOptions, SkiffApp};

type App = Harness<'static, SkiffApp>;

const SIZE: [f32; 2] = [1280.0, 800.0];

fn workspace() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    dir.canonicalize().unwrap_or(dir)
}

fn kubectl(kubeconfig: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("kubectl").arg("--kubeconfig").arg(kubeconfig).args(["--context", "demo"]).args(args).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn has(h: &App, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn has_containing(h: &App, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

fn steps(h: &mut App, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

/// Runs frames, without recording them, until `done` holds.
fn settle(h: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        h.step();
        if done(h) {
            for _ in 0..6 {
                std::thread::sleep(Duration::from_millis(30));
                h.step();
            }
            return;
        }
        if Instant::now() >= deadline {
            let dir = workspace().join("target/demo");
            std::fs::create_dir_all(&dir).unwrap();
            h.render().expect("render").save(dir.join("timeout.png")).unwrap();
            panic!("timed out waiting for: {what} (see target/demo/timeout.png)");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Writes the frames and the manifest.
struct Recorder {
    dir: PathBuf,
    manifest: std::fs::File,
    frames: usize,
    pointer: Option<egui::Pos2>,
}

impl Recorder {
    fn new() -> Self {
        let dir = workspace().join("target/demo/frames");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = std::fs::File::create(dir.join("manifest.jsonl")).unwrap();
        let mut rec = Self { dir, manifest, frames: 0, pointer: None };
        rec.line(serde_json::json!({ "size": SIZE }));
        rec
    }

    fn line(&mut self, value: serde_json::Value) {
        writeln!(self.manifest, "{value}").unwrap();
    }

    /// A new scene: the caption under the picture changes, and the render
    /// step fades from the last frame to the next one.
    fn scene(&mut self, caption: &str) {
        self.line(serde_json::json!({ "scene": caption }));
    }

    fn write(&mut self, h: &mut App, ms: u64, glide: bool, click: bool) {
        self.frames += 1;
        let file = format!("{:04}.png", self.frames);
        h.render().expect("render").save(self.dir.join(&file)).unwrap();
        let pointer = self.pointer.map(|p| [p.x.round(), p.y.round()]);
        self.line(serde_json::json!({ "file": file, "ms": ms, "pointer": pointer, "glide": glide, "click": click }));
    }

    /// What the app shows now, on screen for `ms`.
    fn frame(&mut self, h: &mut App, ms: u64) {
        h.step();
        self.write(h, ms, false, false);
    }

    /// Moves the pointer to `to`: one frame during which the render step
    /// slides the drawn pointer there, then the app is told, so that what
    /// reacts to the pointer (highlights, tooltips) follows its arrival.
    fn point(&mut self, h: &mut App, to: egui::Pos2) {
        self.pointer = Some(to);
        self.write(h, 420, true, false);
        h.hover_at(to);
        steps(h, 2);
    }

    /// Points at `to` and clicks there, with the real pointer events.
    fn click(&mut self, h: &mut App, to: egui::Pos2) {
        self.point(h, to);
        self.write(h, 160, false, false);
        h.drag_at(to);
        h.step();
        self.write(h, 220, false, true);
        h.drop_at(to);
        steps(h, 2);
    }

    /// Drags from where the pointer is to `to`, in small steps.
    fn drag(&mut self, h: &mut App, to: egui::Pos2) {
        let from = self.pointer.expect("point at the thing first");
        self.write(h, 160, false, false);
        h.drag_at(from);
        h.step();
        self.write(h, 200, false, true);
        let n = 9;
        for i in 1..=n {
            // (Ease out: fast at first, slower on arrival.)
            let t = 1.0 - (1.0 - i as f32 / n as f32).powi(2);
            let at = from + (to - from) * t;
            h.hover_at(at);
            steps(h, 2);
            self.pointer = Some(at);
            self.write(h, 70, false, false);
        }
        h.drop_at(to);
        steps(h, 2);
    }

    /// Types `text` into the text field that has the keyboard, a
    /// character per frame.
    fn type_in(&mut self, h: &mut App, text: &str, field: impl Fn(&App) -> egui_kittest::Node<'_>) {
        for (i, c) in text.chars().enumerate() {
            field(h).type_text(&c.to_string());
            steps(h, 2);
            // (An uneven rhythm reads as typing.)
            self.frame(h, if i % 3 == 2 { 130 } else { 85 });
        }
    }
}

fn centre(h: &App, label: &str) -> egui::Pos2 {
    h.get_by_label(label).rect().center()
}

/// The table row called `name`: the leftmost thing with that label.
fn row(h: &App, name: &str) -> egui::Pos2 {
    let rect = h.query_all_by_label(name).min_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).expect("row").rect();
    egui::pos2(rect.left() + 60.0, rect.center().y)
}

fn only_field(h: &App) -> egui_kittest::Node<'_> {
    h.get_by_role(Role::TextInput)
}

fn last_field(h: &App) -> egui_kittest::Node<'_> {
    h.get_all_by_role(Role::TextInput).last().expect("text field")
}

fn terminal(h: &App) -> egui_kittest::Node<'_> {
    h.get_by_label("Terminal")
}

#[test]
#[ignore = "records the demo animation's frames; run by hack/demo/render.sh"]
fn record_demo() {
    let kubeconfig = std::env::var("SKIFF_TEST_KUBECONFIG").map(PathBuf::from).unwrap_or_else(|_| workspace().join("hack/kubeconfig"));
    assert!(kubeconfig.exists(), "no kubeconfig at {}: start the test clusters with hack/cluster-up.sh", kubeconfig.display());
    // SAFETY: the only test in this binary; nothing else reads the environment yet.
    unsafe { std::env::set_var("KUBECONFIG", &kubeconfig) };
    let prometheus = "/api/v1/namespaces/monitoring/services/http:prometheus-operated:9090/proxy/api/v1/query?query=vector(1)";
    assert!(kubectl(&kubeconfig, &["get", "--raw", prometheus]).contains("success"), "the demo cluster has no Prometheus: run hack/cluster-up.sh");

    // The pod the tour opens: a kube-proxy pod, which the test bed's churn
    // leaves alone. Its name ends in a random suffix, so it is typed as
    // far as the dash and then picked from what the filter leaves.
    let pods = kubectl(&kubeconfig, &["-n", "kube-system", "get", "pods", "-o", "name"]);
    let mut pods: Vec<&str> = pods.lines().filter_map(|l| l.strip_prefix("pod/")).filter(|n| n.starts_with("kube-proxy-")).collect();
    pods.sort();
    let pod = pods.first().expect("a kube-proxy pod").to_string();

    // A config file of its own, so that the tour changes nobody's settings.
    let dir = workspace().join("target/demo/config");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");
    std::fs::write(&path, "{}").unwrap();
    let (config, warnings) = skiff::config::Config::load_from(&path);
    let opts = AppOptions { context: Some("demo".into()), view: Some("overview".into()), ..Default::default() };
    let mut h: App = Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(1.0)
        .with_render_cursor(false)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless_with_config(&cc.egui_ctx, opts, config, warnings));
    let h = &mut h;
    let mut rec = Recorder::new();

    // 1. The overview: what is broken, and the alerts that are firing.
    settle(h, "the overview with its alerts", |h| has_containing(h, "FIRING ALERTS · ") && has_containing(h, "NEEDS ATTENTION"));
    rec.scene("One window shows what is broken and which alerts are firing");
    rec.frame(h, 1300);
    let alert = h.query_all_by_label("KubeNodeNotReady").next().expect("alert").rect();
    rec.point(h, egui::pos2(alert.center().x, alert.center().y + 4.0));
    rec.frame(h, 600);
    let alert = h.query_all_by_label("KubePodCrashLooping").next().expect("alert").rect();
    rec.point(h, egui::pos2(alert.center().x + 10.0, alert.center().y + 4.0));
    rec.frame(h, 1100);

    // 2. The palette jumps to the pods; the filter narrows as it is typed.
    rec.scene("Ctrl+K jumps to anything, and / filters the table as you type");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    steps(h, 3);
    rec.frame(h, 450);
    rec.type_in(h, "pods", last_field);
    rec.frame(h, 550);
    h.key_press(egui::Key::Enter);
    settle(h, "the pods", |h| h.state().view_id() == "pods" && h.state().table_stats().1 && h.state().table_stats().0 > 0);
    rec.frame(h, 900);
    h.key_press(egui::Key::Slash);
    steps(h, 2);
    rec.frame(h, 400);
    rec.type_in(h, "kube-proxy", only_field);
    settle(h, "the filtered pods", |h| has(h, &pod));
    rec.frame(h, 900);

    // 3. A pod's details, and its metrics from Prometheus.
    rec.scene("Details beside the table, with metrics charted from your Prometheus");
    let at = row(h, &pod);
    rec.click(h, at);
    settle(h, "the detail panel", |h| has(h, "CONTAINERS") && has(h, "Metrics"));
    rec.frame(h, 1900);
    let at = centre(h, "Metrics");
    rec.click(h, at);
    settle(h, "the charts", |h| has_containing(h, "one point every"));
    rec.frame(h, 1100);
    // The pointer over a chart reads out the values at that moment.
    let chart = h.query_all_by_label("CPU").max_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).expect("chart").rect();
    for across in [150.0, 215.0, 280.0] {
        rec.point(h, egui::pos2(chart.left() + across, chart.bottom() + 80.0));
        rec.frame(h, 350);
    }
    rec.frame(h, 800);

    // 4. The logs, following.
    rec.scene("Logs follow live in a dock under the table");
    let at = centre(h, "Logs");
    rec.click(h, at);
    settle(h, "log lines", |h| has_containing(h, " lines") && has_containing(h, "streaming"));
    rec.frame(h, 600);
    // The dock's upper edge is a handle: more room for the lines.
    let tab = h.query_all_by_label_contains(&pod).filter(|n| n.rect().top() > 400.0).map(|n| n.rect()).next().expect("the dock's tab");
    let edge = egui::pos2(640.0, tab.top() - 9.0);
    rec.point(h, edge);
    rec.drag(h, egui::pos2(640.0, 372.0));
    let quiet = egui::pos2(520.0, 560.0);
    rec.point(h, quiet);
    // (Real time: the lines are written by hack/logsim.sh as this runs.)
    for _ in 0..22 {
        std::thread::sleep(Duration::from_millis(120));
        rec.frame(h, 120);
    }
    rec.frame(h, 300);

    // 5. A shell in the container.
    rec.scene("A shell in any container, one click away");
    let at = centre(h, "Shell");
    rec.click(h, at);
    settle(h, "the shell", |h| has(h, "connected"));
    let screen = |h: &App| h.state().dock.shell_screen().unwrap_or_default();
    // (A shell's tab starts at the dock's smallest height: pulled up
    // again, as for the logs, without recording it a second time.)
    let tab = h.query_all_by_label_contains(&pod).filter(|n| n.rect().top() > 400.0).map(|n| n.rect()).next().expect("the dock's tab");
    let edge = egui::pos2(640.0, tab.top() - 9.0);
    h.hover_at(edge);
    steps(h, 2);
    h.drag_at(edge);
    steps(h, 2);
    h.hover_at(egui::pos2(640.0, 372.0));
    steps(h, 2);
    h.drop_at(egui::pos2(640.0, 372.0));
    steps(h, 2);
    h.hover_at(quiet);
    steps(h, 2);
    // (The test bed runs the shell on this computer and bash complains
    // about that when it starts: cleared before anything is recorded.)
    terminal(h).type_text("clear");
    h.key_press(egui::Key::Enter);
    settle(h, "a cleared screen", |h| screen(h).lines().filter(|l| !l.trim().is_empty()).count() == 1);
    rec.point(h, quiet);
    rec.frame(h, 500);
    for (command, output) in [("id", "uid="), ("date -u", " UTC ")] {
        rec.type_in(h, command, terminal);
        rec.frame(h, 350);
        h.key_press(egui::Key::Enter);
        settle(h, "the command's output", |h| screen(h).lines().any(|l| l.contains(output) && !l.contains('$')));
        rec.frame(h, 800);
    }
    rec.frame(h, 400);

    // 6. Read-only: the cluster cannot be changed from Skiff.
    rec.scene("Mark a cluster read-only and Skiff will not change anything in it");
    // (Ctrl+K belongs to the shell while it has the keyboard; with Shift
    // it is the palette's everywhere, except on a Mac.)
    let palette = if h.ctx.os() == egui::os::OperatingSystem::Mac { egui::Modifiers::COMMAND } else { egui::Modifiers::COMMAND | egui::Modifiers::SHIFT };
    h.key_press_modifiers(palette, egui::Key::K);
    steps(h, 3);
    rec.frame(h, 400);
    rec.type_in(h, "read-only", last_field);
    assert!(has(h, "Make demo read-only"));
    rec.frame(h, 600);
    h.key_press(egui::Key::Enter);
    settle(h, "read-only", |h| h.state().read_only() && has(h, "Read-only"));
    rec.frame(h, 1100);
    // The actions that change things are greyed out, and say why.
    let at = centre(h, "Delete");
    rec.point(h, at);
    settle(h, "the reason", |h| has_containing(h, "is read-only in Skiff. Turn that off"));
    rec.frame(h, 2100);
    h.hover_at(quiet);
    steps(h, 3);

    // 7. Settings.
    rec.scene("Settings in a window, saved to a plain JSON file");
    let at = h.get_by_label_contains("Settings  (").rect().center();
    rec.click(h, at);
    settle(h, "the settings window", |h| has(h, "Close settings"));
    rec.frame(h, 1200);
    let at = centre(h, "Light");
    rec.click(h, at);
    settle(h, "the light theme", |h| !h.state().pal.dark);
    rec.frame(h, 1100);
    let at = centre(h, "Dark");
    rec.click(h, at);
    settle(h, "the dark theme", |h| h.state().pal.dark);
    rec.frame(h, 600);
    // (The shell is called "Terminal" too: the section's button is the smallest thing of that name.)
    let area = |r: egui::Rect| r.width() * r.height();
    let at = h.query_all_by_label("Terminal").map(|n| n.rect()).min_by(|a, b| area(*a).total_cmp(&area(*b))).expect("section").center();
    rec.click(h, at);
    steps(h, 4);
    rec.frame(h, 1800);

    println!("{} frames in {}", rec.frames, rec.dir.display());
}
