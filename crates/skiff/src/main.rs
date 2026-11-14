#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use skiff::{AppOptions, SkiffApp};

const USAGE: &str = "\
Skiff - a fast, native Kubernetes cluster browser

USAGE:
    skiff [OPTIONS]

OPTIONS:
    -c, --context <NAME>   Open this kubeconfig context (default: last used / current)
    -n, --namespace <NS>   Start with only this namespace selected
        --view <RESOURCE>  Start on a resource view, e.g. pods or deployments.apps
        --light            Use the light theme
    -h, --help             Show this help
    -V, --version          Show the version

Clusters come from $KUBECONFIG or ~/.kube/config, exactly like kubectl.";

fn parse_args() -> Result<AppOptions, String> {
    let mut opts = AppOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--context" => opts.context = Some(args.next().ok_or("--context needs a value")?),
            "-n" | "--namespace" => opts.namespace = Some(args.next().ok_or("--namespace needs a value")?),
            "--view" => opts.view = Some(args.next().ok_or("--view needs a value")?),
            "--light" => opts.light = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("skiff {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    Ok(opts)
}

fn main() -> eframe::Result<()> {
    skiff::app::mark_process_start();
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Skiff")
            .with_app_id("skiff")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("skiff", native, Box::new(move |cc| Ok(Box::new(SkiffApp::new(cc, opts)))))
}
