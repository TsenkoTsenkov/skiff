//! Skiff: a fast, native Kubernetes cluster browser.

pub mod app;
pub mod chart;
pub mod config;
pub mod detail;
pub mod dock;
pub mod external;
pub mod help;
pub mod keymap;
pub mod log_text;
pub mod metrics_view;
pub mod motion;
pub mod overview;
pub mod pages;
pub mod palette;
pub mod settings;
pub mod shell;
pub mod shell_env;
pub mod table_view;
pub mod theme;
pub mod update;
pub mod widgets;

pub use app::{AppOptions, NavFocus, Notice, NoticeAction, SkiffApp, View};
