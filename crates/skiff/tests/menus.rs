//! Menus that need no cluster: run against a kubeconfig whose servers are
//! unreachable, with no GPU (nothing is rendered, only laid out).

use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use skiff::{AppOptions, SkiffApp};

const NAMES: [&str; 11] = [
    "aks-2bcloud",
    "kubernetes-admin",
    "mm-ak-system",
    "mm-ak-system3",
    "mm-aks-system",
    "mm-aks-system2",
    "mm-dev-services",
    "mm-prod-services",
    "mmwesteuropeprodaks",
    "mmwesteuropetestaks",
    "kubernetes-admin@kubernetes",
];

fn fake_kubeconfig() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("skiff-menus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut y = String::from("apiVersion: v1\nkind: Config\nclusters:\n");
    for n in NAMES {
        y.push_str(&format!("- name: {n}\n  cluster:\n    server: https://127.0.0.1:1\n"));
    }
    y.push_str("users:\n- name: u\n  user:\n    token: x\ncontexts:\n");
    for n in NAMES {
        y.push_str(&format!("- name: {n}\n  context:\n    cluster: {n}\n    user: u\n"));
    }
    y.push_str(&format!("current-context: {}\n", NAMES[8]));
    let path = dir.join("config");
    std::fs::write(&path, y).unwrap();
    path
}

fn row_width(h: &Harness<'static, SkiffApp>, name: &str) -> f32 {
    let node = h
        .get_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().as_deref() == Some(name) && n.rect().width() > 100.0)
        .unwrap_or_else(|| panic!("no menu row for {name}"));
    node.rect().width() as f32
}

fn open_switcher(h: &Harness<'static, SkiffApp>, current: &str) {
    h.get_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().is_some_and(|l| l.contains(current)) && n.rect().top() < 50.0)
        .expect("the cluster switcher")
        .click();
}

#[test]
fn cluster_menu_keeps_its_size() {
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("KUBECONFIG", fake_kubeconfig()) };
    let opts = AppOptions { context: Some(NAMES[8].to_string()), view: None, light: false, namespace: None };
    let mut h = Harness::builder()
        .with_size([1400.0, 800.0])
        .with_pixels_per_point(1.0)
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts));
    h.step();
    open_switcher(&h, NAMES[8]);
    for _ in 0..5 {
        h.step();
    }
    let first = row_width(&h, NAMES[0]);
    // Hover and connect to a few clusters (each adds a disconnect button),
    // reopening the menu in between, as a user picking clusters would.
    for name in [NAMES[2], NAMES[3], NAMES[5]] {
        h.get_all_by_role(Role::Button)
            .find(|n| n.accesskit_node().label().as_deref() == Some(name) && n.rect().width() > 100.0)
            .unwrap()
            .click();
        for _ in 0..5 {
            h.step();
        }
        open_switcher(&h, name);
        for _ in 0..20 {
            h.step();
        }
        let w = row_width(&h, NAMES[0]);
        assert!((w - first).abs() < 1.0, "menu grew from {first} to {w} after picking {name}");
    }
    assert!(first < 700.0, "menu is {first} wide");
}
