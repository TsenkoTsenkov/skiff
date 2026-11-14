//! Skiff: a fast, native Kubernetes cluster browser.

pub mod app;
pub mod detail;
pub mod dock;
pub mod external;
pub mod log_text;
pub mod overview;
pub mod palette;
pub mod shell;
pub mod shell_env;
pub mod table_view;
pub mod theme;
pub mod widgets;

pub use app::{AppOptions, SkiffApp, View};
