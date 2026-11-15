//! Headless widget tests: egui_kittest + wgpu (lavapipe on CI/containers).
//!
//! PNGs are written to `$CARGO_TARGET_TMPDIR/skiff-term-screenshots/` (i.e. `<target>/tmp/...`) for review.

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{Event, FontData, FontFamily, Key, Modifiers, Pos2, Vec2};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use skiff_term::{TermOutput, TermStyle, TerminalView};

const JBM: &[u8] = include_bytes!("../../skiff/assets/fonts/JetBrainsMono-Regular.ttf");
/// Optional CJK fallback from the system (Ubuntu: fonts-wqy-zenhei).
const CJK: &str = "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc";

struct Host {
    term: TerminalView,
    style: TermStyle,
    input: Vec<u8>,
    resizes: Vec<(u16, u16)>,
    focus: bool,
}

fn setup_fonts(ctx: &egui::Context) {
    ctx.add_font(FontInsert::new(
        "JetBrainsMono",
        FontData::from_static(JBM),
        vec![InsertFontFamily {
            family: FontFamily::Monospace,
            priority: FontPriority::Highest,
        }],
    ));
    if let Ok(bytes) = std::fs::read(CJK) {
        ctx.add_font(FontInsert::new(
            "wqy",
            FontData::from_owned(bytes),
            vec![InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            }],
        ));
    }
}

fn harness(size: Vec2, with_button: bool) -> Harness<'static, Host> {
    let mut b = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .with_context_setup(setup_fonts);
    if !std::path::Path::new(CJK).exists() {
        b = b.allow_missing_glyphs();
    }
    b.wgpu().build_ui_state(
        move |ui, host: &mut Host| {
            if with_button {
                let _ = ui.button("Other widget");
            }
            let out: TermOutput = host.term.ui(ui, &host.style);
            host.input.extend_from_slice(&out.input);
            if let Some(r) = out.resize {
                host.resizes.push(r);
            }
            host.focus = out.has_focus;
        },
        Host {
            term: TerminalView::new(),
            style: TermStyle::default(),
            input: Vec::new(),
            resizes: Vec::new(),
            focus: false,
        },
    )
}

fn save(h: &mut Harness<'_, Host>, name: &str) -> String {
    let img = h.render().expect("render");
    let dir = concat!(env!("CARGO_TARGET_TMPDIR"), "/skiff-term-screenshots");
    std::fs::create_dir_all(dir).unwrap();
    let path = format!("{dir}/{name}.png");
    img.save(&path).unwrap();
    println!("wrote {path}");
    path
}

/// A realistic interactive shell session.
const SESSION: &[&[u8]] = &[
    b"\x1b]0;api-7f9c4d: /app\x07",
    b"\x1b[01;32mroot@api-7f9c4d\x1b[00m:\x1b[01;34m/app\x1b[00m# ls --color=auto -la\r\n",
    b"total 48\r\n",
    b"drwxr-xr-x 1 root root 4096 Nov 14 10:02 \x1b[01;34m.\x1b[0m\r\n",
    b"drwxr-xr-x 1 root root 4096 Nov 14 09:58 \x1b[01;34m..\x1b[0m\r\n",
    b"-rwxr-xr-x 1 root root 8.1M Nov 14 10:01 \x1b[01;32mserver\x1b[0m\r\n",
    b"lrwxrwxrwx 1 root root   19 Nov 14 10:01 \x1b[01;36mcurrent\x1b[0m -> /app/releases/v1.4\r\n",
    b"-rw-r--r-- 1 root root 2.3K Nov 14 10:01 \x1b[01;31mbackup.tar.gz\x1b[0m\r\n",
    b"-rw-r--r-- 1 root root  812 Nov 14 10:01 config.yaml\r\n",
    b"\x1b[01;32mroot@api-7f9c4d\x1b[00m:\x1b[01;34m/app\x1b[00m# cat notes.txt\r\n",
    "UTF-8: café naïve ✓ → λ   CJK: 日本語 中文 (wide)\r\n".as_bytes(),
    b"2026-11-14T10:02:11Z INFO request handled method=GET path=/api/v1/namespaces/payments/pods?labelSelector=app%3Dapi&limit=500 status=200 dur=12ms\r\n",
    b"styles: \x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[7minverse\x1b[0m \x1b[9mstrike\x1b[0m\r\n",
    b"256: \x1b[38;5;208morange\x1b[0m \x1b[48;5;24m bg24 \x1b[0m \x1b[38;5;245mgray245\x1b[0m  true: \x1b[38;2;255;105;180mhotpink\x1b[0m \x1b[48;2;40;80;40m green-bg \x1b[0m\r\n",
    b"\x1b[01;32mroot@api-7f9c4d\x1b[00m:\x1b[01;34m/app\x1b[00m# ",
];

#[test]
fn renders_realistic_session() {
    let mut h = harness(Vec2::new(960.0, 420.0), false);
    h.run();
    for chunk in SESSION {
        h.state_mut().term.feed(chunk);
    }
    h.run();
    let t = &h.state().term;
    assert_eq!(t.title(), Some("api-7f9c4d: /app"));
    assert_eq!(t.row_text(0), "root@api-7f9c4d:/app# ls --color=auto -la");
    // The 140+ char log line wraps onto a second row.
    let (cols, _) = t.size();
    let log_row = (0..30)
        .find(|r| t.row_text(*r).starts_with("2026-11-14"))
        .unwrap();
    assert_eq!(t.row_text(log_row).chars().count(), cols as usize);
    assert!(t.row_text(log_row + 1).contains("status=200"));
    save(&mut h, "term_unfocused");

    // Click to focus -> solid block cursor.
    h.get_by_label("Terminal").click();
    h.run();
    assert!(h.state().focus);
    h.remove_cursor();
    h.run();
    save(&mut h, "term_focused");

    // Drag-select the "server" filename row and render the selection.
    let r = h.get_by_label("Terminal").rect();
    let (cw, ch) = cell_size(&h);
    let row = 4.0; // "-rwxr-xr-x ... server"
    let y = r.min.y + 4.0 + ch * (row + 0.5);
    let x0 = r.min.x + 4.0 + cw * 0.2;
    let x1 = r.min.x + 4.0 + cw * 47.9;
    h.hover_at(Pos2::new(x0, y));
    h.drag_at(Pos2::new(x0, y));
    h.run();
    h.hover_at(Pos2::new(x1, y));
    h.run();
    h.drop_at(Pos2::new(x1, y));
    h.run();
    let sel = h.state().term.selection_text();
    assert_eq!(
        sel.as_deref(),
        Some("-rwxr-xr-x 1 root root 8.1M Nov 14 10:01 server")
    );
    save(&mut h, "term_selection");

    // Ctrl+Shift+C puts the selection on the clipboard (OutputCommand::CopyText).
    // `step()` runs one frame per queued event and keeps only the last frame's output
    // (`key_down_modifiers` also queues ModifiersChanged), so send the bare key event.
    let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
    h.event(Event::Key {
        key: Key::C,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: ctrl_shift,
    });
    h.step();
    let copied = h
        .output()
        .platform_output
        .commands
        .iter()
        .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t.ends_with("server")));
    assert!(copied, "expected CopyText command");
    h.key_up_modifiers(ctrl_shift, Key::C);
    h.run();
    assert!(h.state().input.is_empty(), "copy must not send bytes");
}

fn cell_size(h: &Harness<'_, Host>) -> (f32, f32) {
    let font = h.state().style.font.clone();
    h.ctx
        .fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)))
}

#[test]
fn keyboard_produces_bytes() {
    let mut h = harness(Vec2::new(700.0, 300.0), true);
    h.run();
    h.get_by_label("Terminal").click();
    h.run();
    assert!(h.state().focus);
    h.state_mut().input.clear();

    let mut expect = Vec::<u8>::new();
    let mut check = |h: &mut Harness<'_, Host>, want: &[u8], what: &str| {
        h.run();
        expect.extend_from_slice(want);
        assert_eq!(
            String::from_utf8_lossy(&h.state().input),
            String::from_utf8_lossy(&expect),
            "after {what}"
        );
        assert!(h.state().focus, "focus lost after {what}");
    };

    h.event(Event::Text("ls -la".into()));
    check(&mut h, b"ls -la", "text");
    h.key_press(Key::Enter);
    check(&mut h, b"\r", "Enter");
    h.key_press(Key::Tab);
    check(&mut h, b"\t", "Tab (must not move egui focus)");
    h.key_press_modifiers(Modifiers::SHIFT, Key::Tab);
    check(&mut h, b"\x1b[Z", "Shift+Tab");
    h.key_press(Key::ArrowUp);
    check(&mut h, b"\x1b[A", "ArrowUp");
    h.key_press(Key::ArrowLeft);
    check(&mut h, b"\x1b[D", "ArrowLeft");
    h.key_press(Key::Escape);
    check(&mut h, b"\x1b", "Escape (must keep focus)");
    h.key_press(Key::Backspace);
    check(&mut h, b"\x7f", "Backspace");
    h.key_press_modifiers(Modifiers::CTRL, Key::C);
    check(&mut h, b"\x03", "Ctrl+C key");
    h.event(Event::Copy); // what eframe sends for Ctrl+C on Linux
    check(&mut h, b"\x03", "Event::Copy");
    h.key_press_modifiers(Modifiers::CTRL, Key::D);
    check(&mut h, b"\x04", "Ctrl+D");
    h.key_press(Key::F5);
    check(&mut h, b"\x1b[15~", "F5");
    h.key_press(Key::Delete);
    check(&mut h, b"\x1b[3~", "Delete");

    // Application cursor mode (e.g. vim / less) switches arrows to SS3.
    h.state_mut().term.feed(b"\x1b[?1h");
    h.key_press(Key::ArrowUp);
    check(&mut h, b"\x1bOA", "ArrowUp in DECCKM");
    h.state_mut().term.feed(b"\x1b[?1l");

    // Paste: plain, then bracketed.
    h.event(Event::Paste("echo a\necho b".into()));
    check(&mut h, b"echo a\recho b", "paste");
    h.state_mut().term.feed(b"\x1b[?2004h");
    h.event(Event::Paste("x".into()));
    check(&mut h, b"\x1b[200~x\x1b[201~", "bracketed paste");

    // A device-status request from the remote is answered through `input`.
    h.state_mut().term.feed(b"\x1b[6n");
    check(&mut h, b"\x1b[1;1R", "DSR reply");
}

#[test]
fn resize_is_reported_and_wheel_scrolls() {
    let mut h = harness(Vec2::new(640.0, 320.0), false);
    h.run();
    let first = *h.state().resizes.first().expect("initial resize");
    assert_eq!(h.state().resizes.len(), 1);
    assert_eq!(first, h.state().term.size());

    h.set_size(Vec2::new(900.0, 500.0));
    h.run();
    let resizes = &h.state().resizes;
    assert_eq!(resizes.len(), 2, "{resizes:?}");
    let second = resizes[1];
    assert!(
        second.0 > first.0 && second.1 > first.1,
        "{first:?} -> {second:?}"
    );
    println!("resize {first:?} -> {second:?}");

    // Mouse wheel scrolls into history; a key press snaps back to the bottom.
    for i in 0..200 {
        h.state_mut().term.feed(format!("line {i}\r\n").as_bytes());
    }
    let r = h.get_by_label("Terminal").rect();
    h.hover_at(r.center());
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: Vec2::new(0.0, 2.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    h.run();
    assert_eq!(h.state().term.display_offset(), 6);
    save(&mut h, "term_scrolled_back");
    h.get_by_label("Terminal").click();
    h.run();
    h.event(Event::Text("q".into()));
    h.run();
    assert_eq!(h.state().term.display_offset(), 0);
}

#[test]
fn find_walks_through_the_scrollback() {
    let mut t = skiff_term::TerminalView::new();
    for i in 0..60 {
        t.feed(format!("line {i}{}\r\n", if i % 20 == 5 { " needle" } else { "" }).as_bytes());
    }
    // Matches on lines 5, 25 and 45; a new search starts at the newest.
    let r = t.find("NEEDLE", true);
    assert!(r.is_none(), "a capital letter makes the search case-sensitive");
    let r = t.find("needle", true).expect("found");
    assert_eq!((r.index, r.total), (3, 3));
    assert_eq!(t.selection_text().as_deref(), Some("needle"));
    let r = t.find("needle", true).expect("older");
    assert_eq!(r.index, 2);
    // Scrolled back to show it.
    assert!(t.display_offset() > 0 || (0..24).any(|row| t.row_text(row) == "line 25 needle"));
    let r = t.find("needle", true).expect("oldest");
    assert_eq!(r.index, 1);
    let r = t.find("needle", true).expect("wraps");
    assert_eq!(r.index, 3);
    let r = t.find("needle", false).expect("newer wraps to the oldest");
    assert_eq!(r.index, 1);
    // Regex characters are taken literally.
    t.feed(b"a.b (x)\r\n");
    assert!(t.find("(x)", true).is_some());
    assert!(t.find("a.b", true).is_some());
    t.feed(b"axb\r\n");
    assert_eq!(t.find("a.b", true).map(|r| r.total), Some(1), "the dot is not a wildcard");
    t.end_find();
    assert_eq!(t.selection_text(), None);
}
