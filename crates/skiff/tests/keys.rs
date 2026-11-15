//! Keyboard navigation between the sidebar and the table, with the default
//! (vim-style) bindings and with custom ones. Needs no cluster: the
//! kubeconfig points at a server that does not answer.

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use skiff::keymap::Keymap;
use skiff::{AppOptions, NavFocus, SkiffApp};

fn fake_kubeconfig() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("skiff-keys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let y = "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:1\n\
             users:\n- name: u\n  user:\n    token: x\ncontexts:\n- name: c\n  context:\n    cluster: c\n    user: u\n\
             current-context: c\n";
    let path = dir.join("config");
    std::fs::write(&path, y).unwrap();
    path
}

fn harness(keymap: Option<&str>) -> Harness<'static, SkiffApp> {
    // SAFETY: set before any thread reads it; every test sets the same value.
    unsafe { std::env::set_var("KUBECONFIG", fake_kubeconfig()) };
    let opts = AppOptions { context: Some("c".into()), ..Default::default() };
    let keymap = keymap.map(|k| {
        let (map, errors) = Keymap::from_json(k);
        assert!(errors.is_empty(), "{errors:?}");
        map
    });
    let mut h = Harness::builder()
        .with_size([1400.0, 800.0])
        .with_pixels_per_point(1.0)
        .build_eframe(move |cc| {
            let mut app = SkiffApp::headless(&cc.egui_ctx, opts);
            if let Some(k) = keymap {
                app.keymap = k;
            }
            app
        });
    h.step();
    h
}

fn press(h: &mut Harness<'static, SkiffApp>, key: Key) {
    h.key_press(key);
    h.step();
    h.step();
}

#[test]
fn vim_keys_move_between_sidebar_and_table() {
    let mut h = harness(None);
    assert_eq!(h.state().view_id(), "overview");
    assert_eq!(h.state().nav_focus, NavFocus::Main);

    // h: into the sidebar; j j: down to Namespaces; l: open it.
    press(&mut h, Key::H);
    assert_eq!(h.state().nav_focus, NavFocus::Sidebar);
    press(&mut h, Key::J);
    press(&mut h, Key::J);
    // Moving the cursor alone does not switch views.
    assert_eq!(h.state().view_id(), "overview");
    press(&mut h, Key::L);
    assert_eq!(h.state().view_id(), "namespaces");
    assert_eq!(h.state().nav_focus, NavFocus::Main);

    // Back in, k up to Nodes, Enter opens it.
    press(&mut h, Key::H);
    press(&mut h, Key::K);
    press(&mut h, Key::Enter);
    assert_eq!(h.state().view_id(), "nodes");

    // g goes to the top (Overview), G (Shift+g) to the bottom; Esc leaves
    // the sidebar without switching.
    press(&mut h, Key::H);
    h.key_press_modifiers(Modifiers::SHIFT, Key::G);
    h.step();
    press(&mut h, Key::G);
    press(&mut h, Key::Escape);
    assert_eq!(h.state().nav_focus, NavFocus::Main);
    assert_eq!(h.state().view_id(), "nodes");

    press(&mut h, Key::H);
    press(&mut h, Key::G);
    press(&mut h, Key::L);
    assert_eq!(h.state().view_id(), "overview");
}

#[test]
fn custom_bindings_replace_the_defaults() {
    // Colemak-style: n/e down/up, left/right on Ctrl+h / Ctrl+l.
    let mut h = harness(Some(r#"{"down": ["n"], "up": ["e"], "focus_left": ["Ctrl+h"], "focus_right": ["Ctrl+l"]}"#));
    press(&mut h, Key::H);
    assert_eq!(h.state().nav_focus, NavFocus::Main, "plain h is no longer bound");
    h.key_press_modifiers(Modifiers::CTRL, Key::H);
    h.step();
    assert_eq!(h.state().nav_focus, NavFocus::Sidebar);
    press(&mut h, Key::J);
    press(&mut h, Key::N);
    press(&mut h, Key::N);
    press(&mut h, Key::E);
    press(&mut h, Key::L);
    assert_eq!(h.state().nav_focus, NavFocus::Sidebar, "plain l is no longer bound");
    h.key_press_modifiers(Modifiers::CTRL, Key::L);
    h.step();
    assert_eq!(h.state().view_id(), "nodes");
}
