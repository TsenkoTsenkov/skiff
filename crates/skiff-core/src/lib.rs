//! Skiff's Kubernetes data layer.
//!
//! UI-independent: a [`Core`] owns a small I/O runtime, a [`Session`] is a
//! connection to one cluster, and a [`TableStore`] is a live, watch-backed
//! table for one resource type that publishes immutable [`Snapshot`]s.

pub mod cells;
pub mod exec;
pub mod forward;
pub mod kinds;
pub mod kubeconfig;
pub mod logs;
pub mod ops;
pub mod prom;
mod prom_external;
pub mod session;
pub mod table;
pub mod util;
pub mod views;
pub mod watch;

pub use cells::{fmt_age, fmt_bytes, fmt_cpu, now_unix, Cell, ColKind, Tone};
pub use exec::{ExecSession, ExecState};
pub use forward::PortForward;
pub use kinds::{Builtin, Category, Discovered, ResourceKey, BUILTINS};
pub use kubeconfig::{find_executable, KubeconfigProblem, KubeconfigSources, LoadedKubeconfig};
pub use logs::{LogLine, LogOptions, LogStream, LogTarget, SourceState};
pub use ops::{ContainerInfo, ObjectData};
pub use prom::{
    Alert, AlertKind, AlertTarget, ConnectionTest, ExternalAuth, ExternalEndpoint, Prom, PromConfig, PromEndpoint, PromError, SampleRing,
    SecretSource, TimeRange,
};
pub use session::{
    load_contexts, load_contexts_from, Alerts, ConnStatus, ContextInfo, Contexts, Core, Metrics, PromStatus, Session, Usage,
    UsageHistory,
};
pub use table::{compare, ColSource, Column, LoadState, Row, Snapshot, TableStore};
pub use util::{classify, ErrorInfo, ErrorKind, Job, Notifier};
pub use watch::ListQuery;
