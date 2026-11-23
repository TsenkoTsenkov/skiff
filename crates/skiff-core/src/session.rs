//! A connection to one cluster: client, live tables, metrics, operations.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::FutureExt;
use kube::config::KubeConfigOptions;
use kube::discovery::{verbs, Discovery, Scope};
use kube::{Client, Config};
use parking_lot::Mutex;
use serde::Deserialize;
use tokio::runtime::Handle;
use tokio::sync::watch as tokio_watch;
use tokio::task::AbortHandle;

use crate::cells::{now_unix, parse_quantity};
use crate::exec::{self, ExecSession};
use crate::forward::{self, PortForward};
use crate::kinds::{self, Discovered, ResourceKey};
use crate::kubeconfig::{self, KubeconfigSources, LoadedKubeconfig};
use crate::logs::{self, LogOptions, LogStream, LogTarget};
use crate::ops::{self, ContainerInfo, ObjectData};
use crate::table::{LoadState, Snapshot, TableStore};
use crate::util::{self, classify, classify_kube, friendly, get_request, ErrorInfo, ErrorKind, Job, Notifier};
use crate::watch::{self, ListQuery};

/// Stores nobody has looked at for this long stop watching.
const IDLE_EVICT_SECS: i64 = 180;
/// The same, while the UI is not painting at all (hidden or minimised).
const HIDDEN_EVICT_SECS: i64 = 900;
const METRICS_INTERVAL: Duration = Duration::from_secs(15);
/// Upper bound for one-shot requests (get, delete, scale, ...).
const JOB_TIMEOUT: Duration = ops::REQUEST_TIMEOUT;
/// How long building a client may take. Almost all of it is the kubeconfig's
/// exec credential plugin, which may open a browser and wait for a sign-in.
const CLIENT_BUILD_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextInfo {
    pub name: String,
    pub cluster: String,
    pub user: String,
    pub namespace: Option<String>,
    /// The API server URL as written in the kubeconfig.
    pub server: String,
    /// The connection is not verified: `insecure-skip-tls-verify` is set,
    /// or the server is plain `http://`.
    pub insecure: bool,
    /// The cluster's `proxy-url`, if it has one.
    pub proxy: Option<String>,
    /// Name of the exec credential plugin the user signs in with (`aws`,
    /// `gke-gcloud-auth-plugin`, ...), without its directory.
    pub exec_command: Option<String>,
    /// The kubeconfig file that defined this context.
    pub file: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Contexts {
    pub current: Option<String>,
    pub items: Vec<ContextInfo>,
}

/// Reads the default kubeconfigs (the `$KUBECONFIG` list, else
/// `~/.kube/config`) and lists their contexts. Missing files are skipped;
/// this only fails when no context could be read and a file is broken.
/// [`load_contexts_from`] reports per-file problems instead.
pub fn load_contexts() -> Result<Contexts, String> {
    let loaded = kubeconfig::load(&KubeconfigSources::from_env());
    match loaded.problems.first() {
        Some(problem) if loaded.contexts.items.is_empty() => Err(format!("cannot read kubeconfig {problem}")),
        _ => Ok(loaded.contexts),
    }
}

/// Reads and merges the kubeconfigs of `sources`: contexts, the files that
/// were read, and what is wrong with the ones that could not be.
pub fn load_contexts_from(sources: &KubeconfigSources) -> LoadedKubeconfig {
    kubeconfig::load(sources)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnStatus {
    Connecting,
    Connected { version: String },
    Failed(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// Cores.
    pub cpu: f64,
    /// Bytes.
    pub memory: f64,
}

/// Latest sample from metrics-server (`metrics.k8s.io`).
#[derive(Debug, Default)]
pub struct Metrics {
    /// False until the first successful sample; stays false when the
    /// cluster has no metrics API.
    pub available: bool,
    pub generation: u64,
    /// Keyed by `namespace/name`.
    pub pods: HashMap<Box<str>, Usage>,
    pub nodes: HashMap<Box<str>, Usage>,
}

impl Metrics {
    pub fn pod(&self, namespace: &str, name: &str) -> Option<Usage> {
        let mut k = String::with_capacity(namespace.len() + name.len() + 1);
        k.push_str(namespace);
        k.push('/');
        k.push_str(name);
        self.pods.get(k.as_str()).copied()
    }
    pub fn node(&self, name: &str) -> Option<Usage> {
        self.nodes.get(name).copied()
    }
}

type ClientState = Option<Result<Client, String>>;
/// Live tables by (resource, namespace scope).
type Stores = HashMap<(ResourceKey, Option<Box<str>>), Arc<TableStore>>;

pub struct Session {
    pub context: String,
    handle: Handle,
    notify: Notifier,
    client_tx: tokio_watch::Sender<ClientState>,
    status: ArcSwap<ConnStatus>,
    discovered: ArcSwap<Vec<Discovered>>,
    stores: Mutex<Stores>,
    metrics: ArcSwap<Metrics>,
    metrics_wanted: AtomicI64,
    forwards: Mutex<Vec<Arc<PortForward>>>,
    tasks: Mutex<Vec<AbortHandle>>,
    /// Number of connection attempts started.
    conn_gen: AtomicU64,
    /// Serialises "is this attempt still current?" with publishing its
    /// outcome, against `reconnect()` starting a newer attempt.
    conn_lock: Mutex<()>,
    /// Open log streams and shells, so `shutdown()` can end them.
    logs: Mutex<Vec<Weak<LogStream>>>,
    execs: Mutex<Vec<Weak<ExecSession>>>,
    /// Where kubeconfigs are read from; shared with the `Core`, so a
    /// reconnect sees files the user added since.
    sources: Arc<ArcSwap<KubeconfigSources>>,
    /// Why the newest connection attempt failed, in structured form.
    failure: ArcSwap<Option<ErrorInfo>>,
    /// What a connection attempt is waiting for, tagged with its number
    /// so a superseded attempt's text is never shown.
    waiting: Mutex<(u64, Option<String>)>,
    /// Refuse everything that changes the cluster, opens a shell or
    /// forwards a port.
    read_only: AtomicBool,
}

fn kubeconfig_error(summary: String, hint: &str) -> ErrorInfo {
    ErrorInfo::new(ErrorKind::Kubeconfig, summary, Some(hint))
}

/// Builds a client for a kubeconfig context; also returns the server URL.
///
/// Nothing here may block an I/O worker: there are two of them for the
/// whole app, and building a client runs the kubeconfig's exec credential
/// plugin synchronously, which can wait on a browser sign-in. File reading
/// and the plugin therefore run on the blocking pool, with a time limit.
/// `waiting` is told what is being waited for (`None` when done).
async fn build_client(
    context: &str,
    sources: &KubeconfigSources,
    waiting: impl Fn(Option<String>),
) -> Result<(Client, String), ErrorInfo> {
    let (src, ctx) = (sources.clone(), context.to_string());
    // Re-read on every attempt: this is how refreshed credentials and
    // newly added files are picked up.
    let prepared = tokio::task::spawn_blocking(move || {
        let merged = kubeconfig::read_merged(&src);
        if !merged.config.contexts.iter().any(|c| c.name == ctx) {
            let summary = format!("Context {ctx:?} is not in your kubeconfig");
            return Err(match merged.problems.first() {
                Some(p) => kubeconfig_error(
                    format!("{summary} ({} could not be read: {})", p.path.display(), p.message),
                    "Fix that file, or add the right kubeconfig in Skiff, then reload.",
                ),
                None if merged.files.is_empty() => kubeconfig_error(
                    format!("{summary}: no kubeconfig file was found"),
                    "Add a kubeconfig file in Skiff, then reload.",
                ),
                None => kubeconfig_error(
                    summary,
                    "It may have been renamed or removed. Reload the kubeconfig and pick a cluster again.",
                ),
            });
        }
        // A missing plugin is the most common first-run failure: name the
        // command rather than report "No such file or directory".
        let plugin = match kubeconfig::exec_of(&merged.config, &ctx).and_then(|e| Some((e.command.clone()?, e))) {
            Some((command, exec)) => {
                if kubeconfig::find_executable(&command).is_none() {
                    return Err(util::exec_missing(Some(&command), exec.install_hint.as_deref()));
                }
                Some(kubeconfig::command_name(&command))
            }
            None => None,
        };
        Ok((merged.config, plugin))
    })
    .await;
    let (kc, plugin) = match prepared {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(ErrorInfo::other("Skiff hit an internal error reading the kubeconfig")),
    };

    let opts = KubeConfigOptions { context: Some(context.to_string()), ..Default::default() };
    let mut cfg = Config::from_custom_kubeconfig(kc, &opts).await.map_err(|e| {
        kubeconfig_error(
            format!("Kubeconfig context {context:?}: {e}"),
            "Check the cluster and user entries of this context in your kubeconfig.",
        )
    })?;
    // Fail fast on unreachable clusters instead of hanging the UI.
    cfg.connect_timeout = Some(Duration::from_secs(8));
    let server = cfg.cluster_url.to_string();

    if let Some(p) = &plugin {
        waiting(Some(format!("Waiting for {p}…")));
    }
    // `Client::try_from` is where the exec plugin runs.
    let built =
        tokio::time::timeout(CLIENT_BUILD_TIMEOUT, tokio::task::spawn_blocking(move || Client::try_from(cfg))).await;
    waiting(None);
    match built {
        Ok(Ok(Ok(client))) => Ok((client, server)),
        Ok(Ok(Err(e))) => Err(classify_kube(&e, plugin.as_deref())),
        Ok(Err(_)) => Err(ErrorInfo::other("Skiff hit an internal error setting up the connection")),
        // The plugin process cannot be cancelled from here; it is left to
        // finish on its own and its result is dropped.
        Err(_) => Err(util::exec_timeout(plugin.as_deref(), CLIENT_BUILD_TIMEOUT.as_secs())),
    }
}

/// Runs a table's watch for as long as the session has a client, following
/// reconnects. A panic in the watch code must not leave a table that looks
/// alive but never updates: it is published as a failed load, and the task
/// starts over when the session reconnects.
async fn table_task<F, Fut>(
    store: Arc<TableStore>,
    notify: Notifier,
    mut rx: tokio_watch::Receiver<ClientState>,
    run: F,
) where
    F: Fn(Arc<TableStore>, Client, Notifier) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let attempt = std::panic::AssertUnwindSafe(table_loop(&store, &notify, &mut rx, &run)).catch_unwind().await;
        let Err(panic) = attempt else { return };
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let mut snap = Snapshot::empty(LoadState::Failed);
        let mut msg = "Skiff hit an internal error loading this table".to_string();
        if !what.is_empty() {
            msg.push_str(&format!(" ({})", what.chars().take(200).collect::<String>()));
        }
        msg.push_str(" — Reconnect to try again, and please report this.");
        snap.error = Some(msg.into());
        snap.generation = store.peek().generation + 1;
        store.publish(snap);
        notify.ping();
        // Retrying at once would most likely panic again; a reconnect is
        // the user's "try again".
        if rx.changed().await.is_err() {
            return;
        }
    }
}

async fn table_loop<F, Fut>(
    store: &Arc<TableStore>,
    notify: &Notifier,
    rx: &mut tokio_watch::Receiver<ClientState>,
    run: &F,
) where
    F: Fn(Arc<TableStore>, Client, Notifier) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let current = rx.borrow_and_update().clone();
        match current {
            Some(Ok(client)) => {
                tokio::select! {
                    _ = run(store.clone(), client, notify.clone()) => return,
                    changed = rx.changed() => if changed.is_err() { return },
                }
            }
            other => {
                let mut snap = Snapshot::empty(if other.is_some() { LoadState::Failed } else { LoadState::Connecting });
                if let Some(Err(e)) = other {
                    snap.error = Some(e.into());
                }
                snap.generation = store.peek().generation + 1;
                store.publish(snap);
                notify.ping();
                if rx.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn discover(client: &Client) -> Result<Vec<Discovered>, String> {
    // Aggregated discovery is two requests total; the legacy path is one
    // per API group and only used for old servers.
    let discovery = match Discovery::new(client.clone()).run_aggregated().await {
        Ok(d) => d,
        Err(_) => Discovery::new(client.clone()).run().await.map_err(|e| friendly(&e))?,
    };
    let mut out = Vec::new();
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            if !caps.supports_operation(verbs::LIST) {
                continue;
            }
            out.push(Discovered {
                key: ResourceKey::new(
                    &ar.group,
                    &ar.version,
                    &ar.plural,
                    &ar.kind,
                    matches!(caps.scope, Scope::Namespaced),
                ),
                verbs: caps.operations.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.key.group.cmp(&b.key.group).then_with(|| a.key.kind.cmp(&b.key.kind)));
    Ok(out)
}

#[derive(Deserialize)]
struct MetricsList {
    #[serde(default)]
    items: Vec<MetricsItem>,
}

#[derive(Deserialize)]
struct MetricsItem {
    metadata: MetricsMeta,
    #[serde(default)]
    usage: Option<HashMap<String, String>>,
    #[serde(default)]
    containers: Vec<MetricsContainer>,
}

#[derive(Deserialize)]
struct MetricsMeta {
    name: String,
    #[serde(default)]
    namespace: String,
}

#[derive(Deserialize)]
struct MetricsContainer {
    #[serde(default)]
    usage: HashMap<String, String>,
}

fn usage_of(map: &HashMap<String, String>) -> Usage {
    Usage {
        cpu: map.get("cpu").and_then(|v| parse_quantity(v)).unwrap_or(0.0),
        memory: map.get("memory").and_then(|v| parse_quantity(v)).unwrap_or(0.0),
    }
}

async fn sample_metrics(client: &Client, generation: u64) -> Result<Metrics, kube::Error> {
    let nodes: MetricsList =
        client.request(get_request("/apis/metrics.k8s.io/v1beta1/nodes", None).expect("static url")).await?;
    let pods: MetricsList =
        client.request(get_request("/apis/metrics.k8s.io/v1beta1/pods", None).expect("static url")).await?;
    let mut m = Metrics { available: true, generation, ..Default::default() };
    for n in nodes.items {
        if let Some(u) = &n.usage {
            m.nodes.insert(n.metadata.name.into(), usage_of(u));
        }
    }
    for p in pods.items {
        let mut total = Usage::default();
        for c in &p.containers {
            let u = usage_of(&c.usage);
            total.cpu += u.cpu;
            total.memory += u.memory;
        }
        m.pods.insert(format!("{}/{}", p.metadata.namespace, p.metadata.name).into(), total);
    }
    Ok(m)
}

impl Session {
    pub(crate) fn connect(
        handle: Handle,
        notify: Notifier,
        context: &str,
        sources: Arc<ArcSwap<KubeconfigSources>>,
    ) -> Arc<Session> {
        let (client_tx, _) = tokio_watch::channel(None);
        let session = Arc::new(Session {
            context: context.to_string(),
            handle,
            notify,
            client_tx,
            status: ArcSwap::from_pointee(ConnStatus::Connecting),
            discovered: ArcSwap::from_pointee(Vec::new()),
            stores: Mutex::new(HashMap::new()),
            metrics: ArcSwap::from_pointee(Metrics::default()),
            metrics_wanted: AtomicI64::new(0),
            forwards: Mutex::new(Vec::new()),
            tasks: Mutex::new(Vec::new()),
            conn_gen: AtomicU64::new(0),
            conn_lock: Mutex::new(()),
            logs: Mutex::new(Vec::new()),
            execs: Mutex::new(Vec::new()),
            sources,
            failure: ArcSwap::from_pointee(None),
            waiting: Mutex::new((0, None)),
            read_only: AtomicBool::new(false),
        });
        session.start_connect();
        session.start_janitor();
        session.start_metrics();
        session
    }

    fn spawn_tracked(&self, fut: impl Future<Output = ()> + Send + 'static) {
        let h = self.handle.spawn(fut).abort_handle();
        let mut tasks = self.tasks.lock();
        tasks.retain(|t| !t.is_finished());
        tasks.push(h);
    }

    fn start_connect(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let context = self.context.clone();
        // Only the newest attempt may publish its outcome; a slow older one
        // must not overwrite a newer result.
        let attempt = self.conn_gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.spawn_tracked(async move {
            let current = |s: &Session| s.conn_gen.load(Ordering::SeqCst) == attempt;
            let fail = |s: &Session, info: ErrorInfo| {
                let _publishing = s.conn_lock.lock();
                if current(s) {
                    let msg = info.to_string();
                    s.failure.store(Arc::new(Some(info)));
                    s.status.store(Arc::new(ConnStatus::Failed(msg.clone())));
                    s.client_tx.send_replace(Some(Err(msg)));
                    s.notify.ping();
                }
            };
            // Read at the start of every attempt, so a reconnect uses the
            // files the user has configured by now.
            let Some(sources) = weak.upgrade().map(|s| s.sources.load_full()) else {
                return;
            };
            let waiting = {
                let weak = weak.clone();
                move |text: Option<String>| {
                    if let Some(s) = weak.upgrade() {
                        *s.waiting.lock() = (attempt, text);
                        s.notify.ping();
                    }
                }
            };
            let result = build_client(&context, &sources, waiting).await;
            let (client, server) = {
                let Some(s) = weak.upgrade() else { return };
                let publishing = s.conn_lock.lock();
                match result {
                    Ok((c, server)) if current(&s) => {
                        // Hand the client out immediately so tables start
                        // loading in parallel with the version probe.
                        s.client_tx.send_replace(Some(Ok(c.clone())));
                        (c, server)
                    }
                    Ok(_) => return,
                    Err(e) => {
                        drop(publishing);
                        return fail(&s, e);
                    }
                }
            };
            let server = server.trim_end_matches('/').to_string();
            let version = tokio::time::timeout(Duration::from_secs(15), client.apiserver_version()).await;
            {
                let Some(s) = weak.upgrade() else { return };
                match version {
                    Ok(Ok(v)) => {
                        let _publishing = s.conn_lock.lock();
                        if !current(&s) {
                            return;
                        }
                        s.status.store(Arc::new(ConnStatus::Connected { version: v.git_version }));
                        s.notify.ping();
                    }
                    Ok(Err(e)) => return fail(&s, classify(&e).prefixed(&format!("Cannot reach {server}: "))),
                    Err(_) => {
                        let info = ErrorInfo::new(
                            ErrorKind::Timeout,
                            format!("Cannot reach {server}: no response within 15 seconds"),
                            Some("Check your network connection and VPN, then reconnect."),
                        );
                        return fail(&s, info);
                    }
                }
            }
            let found = discover(&client).await;
            if let (Some(s), Ok(found)) = (weak.upgrade(), found) {
                let _publishing = s.conn_lock.lock();
                if current(&s) {
                    s.discovered.store(Arc::new(found));
                    s.notify.ping();
                }
            }
        });
    }

    fn start_janitor(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.spawn_tracked(async move {
            const TICK: Duration = Duration::from_secs(5);
            let mut wall = std::time::SystemTime::now();
            let mut ticks = 0u32;
            loop {
                tokio::time::sleep(TICK).await;
                let Some(s) = weak.upgrade() else { return };

                // The monotonic clock stops while the machine sleeps but the
                // wall clock does not: a jump means we just woke up, and
                // every open connection is probably dead.
                let now = std::time::SystemTime::now();
                let jumped = now.duration_since(wall).map(|d| d > TICK + Duration::from_secs(20)).unwrap_or(false);
                wall = now;
                if jumped {
                    for store in s.stores.lock().values() {
                        store.refresh();
                    }
                }

                ticks += 1;
                if ticks.is_multiple_of(4) {
                    let mut stores = s.stores.lock();
                    // While the UI is painting, tables it stopped asking for
                    // are unused. If nothing is asked for at all the window
                    // is hidden; keep its tables warm for longer.
                    let ui_active = stores.values().any(|st| st.idle_secs() < 15);
                    let limit = if ui_active { IDLE_EVICT_SECS } else { HIDDEN_EVICT_SECS };
                    stores.retain(|_, store| {
                        let keep = store.idle_secs() < limit;
                        if !keep {
                            store.stop();
                        }
                        keep
                    });
                    drop(stores);
                    s.forwards.lock().retain(|f| f.is_active());
                }
            }
        });
    }

    fn start_metrics(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let mut rx = self.client_tx.subscribe();
        self.spawn_tracked(async move {
            let mut generation = 0u64;
            loop {
                // Follow the session's client across reconnects.
                let client = loop {
                    let current = rx.borrow_and_update().clone();
                    match current {
                        Some(Ok(c)) => break c,
                        _ => {
                            // Disconnected or reconnecting: a sample from
                            // the old connection must not look current.
                            match weak.upgrade() {
                                Some(s) if s.metrics.load().available => {
                                    generation = generation.max(s.metrics.load().generation) + 1;
                                    s.metrics.store(Arc::new(Metrics { generation, ..Default::default() }));
                                    s.notify.ping();
                                }
                                Some(_) => {}
                                None => return,
                            }
                            if rx.changed().await.is_err() {
                                return;
                            }
                        }
                    }
                };
                let mut missing_until = 0i64;
                let mut failures = 0u32;
                loop {
                    let wanted = match weak.upgrade() {
                        Some(s) => now_unix() - s.metrics_wanted.load(Ordering::Relaxed) < 30,
                        None => return,
                    };
                    let delay = if wanted && now_unix() >= missing_until {
                        generation = generation.max(weak.upgrade().map_or(0, |s| s.metrics.load().generation)) + 1;
                        // A reconnect must not wait for a sample that is
                        // still in flight on the old connection.
                        let sample = tokio::select! {
                            r = tokio::time::timeout(Duration::from_secs(20), sample_metrics(&client, generation)) => r,
                            changed = rx.changed() => {
                                if changed.is_err() {
                                    return;
                                }
                                break;
                            }
                        };
                        let Some(s) = weak.upgrade() else { return };
                        match sample {
                            Ok(Ok(m)) => {
                                failures = 0;
                                s.metrics.store(Arc::new(m));
                                s.notify.ping();
                            }
                            other => {
                                failures += 1;
                                let no_api = matches!(&other, Ok(Err(kube::Error::Api(st))) if st.code == 404);
                                if no_api {
                                    // No metrics-server here: check back rarely.
                                    missing_until = now_unix() + 120;
                                }
                                // Never present old numbers as current.
                                if (no_api || failures >= 2) && s.metrics.load().available {
                                    s.metrics.store(Arc::new(Metrics { generation, ..Default::default() }));
                                    s.notify.ping();
                                }
                            }
                        }
                        METRICS_INTERVAL
                    } else {
                        Duration::from_secs(1)
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        changed = rx.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            break;
                        }
                    }
                }
            }
        });
    }

    /// Resolves once the connection attempt finished.
    pub fn client(&self) -> impl Future<Output = Result<Client, String>> + Send + 'static {
        let mut rx = self.client_tx.subscribe();
        async move {
            loop {
                if let Some(r) = rx.borrow_and_update().clone() {
                    return r;
                }
                if rx.changed().await.is_err() {
                    return Err("session closed".into());
                }
            }
        }
    }

    pub fn status(&self) -> Arc<ConnStatus> {
        self.status.load_full()
    }

    /// Why the connection failed, split into a summary and a next step.
    /// `Some` exactly while [`Self::status`] is `Failed`; the string in
    /// `ConnStatus::Failed` is this value's `to_string()`.
    pub fn failure(&self) -> Option<ErrorInfo> {
        match &**self.status.load() {
            ConnStatus::Failed(_) => (**self.failure.load()).clone(),
            _ => None,
        }
    }

    /// While connecting: what the attempt is waiting for, when that is
    /// worth showing ("Waiting for aws…" while a credential plugin runs).
    pub fn connecting_detail(&self) -> Option<String> {
        if !matches!(&**self.status.load(), ConnStatus::Connecting) {
            return None;
        }
        let waiting = self.waiting.lock();
        (waiting.0 == self.conn_gen.load(Ordering::SeqCst)).then(|| waiting.1.clone()).flatten()
    }

    /// Makes this cluster read-only (or writable again) from now on:
    /// while set, everything that changes the cluster, opens a shell or
    /// forwards a port is refused before any request is made. Reading,
    /// logs and tables are unaffected. Shells and forwards that are
    /// already open keep running.
    pub fn set_read_only(&self, read_only: bool) {
        self.read_only.store(read_only, Ordering::SeqCst);
        self.notify.ping();
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::SeqCst)
    }

    /// The refusal for a mutating operation, if this session is read-only.
    fn refused(&self) -> Option<String> {
        self.is_read_only().then(|| util::read_only_error(&self.context).summary)
    }

    /// A job for something that changes the cluster: refused outright on a
    /// read-only session, otherwise like [`Self::job_within`].
    fn mutating_job<T, F>(&self, limit: Option<Duration>, f: impl FnOnce(Client) -> F + Send + 'static) -> Job<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, String>> + Send,
    {
        match self.refused() {
            Some(msg) => Job::done(Err(msg)),
            None => self.job_within(limit, f),
        }
    }

    /// Drops the current client and connects again (after a failure, or to
    /// pick up refreshed credentials).
    pub fn reconnect(self: &Arc<Self>) {
        {
            // Invalidate every earlier attempt before publishing anything,
            // under the lock those attempts publish under.
            let _publishing = self.conn_lock.lock();
            self.conn_gen.fetch_add(1, Ordering::SeqCst);
            self.status.store(Arc::new(ConnStatus::Connecting));
            self.failure.store(Arc::new(None));
            // `send_replace` stores the value even when nobody is subscribed
            // right now; `send` would silently keep the old client.
            self.client_tx.send_replace(None);
            // Numbers from the old connection are not current any more.
            let old = self.metrics.load();
            if old.available {
                self.metrics.store(Arc::new(Metrics { generation: old.generation + 1, ..Default::default() }));
            }
        }
        self.start_connect();
        self.notify.ping();
    }

    /// Reads the session's current client without waiting; `None` while
    /// disconnected. Long-lived streams call this on every (re)connect so
    /// they follow a `reconnect()`.
    fn client_provider(&self) -> logs::ClientProvider {
        let rx = self.client_tx.subscribe();
        Arc::new(move || rx.borrow().clone().and_then(|r| r.ok()))
    }

    /// Everything the API server can list (empty until discovery finishes).
    pub fn discovered(&self) -> Arc<Vec<Discovered>> {
        self.discovered.load_full()
    }

    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics_wanted.store(now_unix(), Ordering::Relaxed);
        self.metrics.load_full()
    }

    /// The live table for a resource type across all namespaces. Starts the
    /// watch on first use; call again each frame rather than caching, so
    /// idle tables can be evicted.
    pub fn table(&self, key: &ResourceKey) -> Arc<TableStore> {
        self.table_in(key, None)
    }

    /// A live table restricted to one namespace (for accounts that may not
    /// list cluster-wide).
    pub fn table_in(&self, key: &ResourceKey, namespace: Option<&str>) -> Arc<TableStore> {
        let scope: Option<Box<str>> = namespace.filter(|n| !n.is_empty() && key.namespaced).map(Into::into);
        let map_key = (key.clone(), scope.clone());
        let mut stores = self.stores.lock();
        if let Some(s) = stores.get(&map_key) {
            return s.clone();
        }
        let store = Arc::new(TableStore::new(key.clone(), scope));
        let rx = self.client_tx.subscribe();
        let task = self.handle.spawn(table_task(store.clone(), self.notify.clone(), rx, watch::run));
        *store.abort.lock() = Some(task.abort_handle());
        stores.insert(map_key, store.clone());
        store
    }

    /// Tables that are currently loaded, without marking them as used.
    pub fn active_tables(&self) -> Vec<Arc<TableStore>> {
        self.stores.lock().values().cloned().collect()
    }

    /// Number of tables currently being watched.
    pub fn active_watches(&self) -> usize {
        self.stores.lock().len()
    }

    fn job<T, F>(&self, f: impl FnOnce(Client) -> F + Send + 'static) -> Job<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, String>> + Send,
    {
        self.job_within(Some(JOB_TIMEOUT), f)
    }

    /// `limit: None` is for work that bounds each of its requests itself.
    fn job_within<T, F>(&self, limit: Option<Duration>, f: impl FnOnce(Client) -> F + Send + 'static) -> Job<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, String>> + Send,
    {
        let job = Job::new();
        let (j, notify, client) = (job.clone(), self.notify.clone(), self.client());
        self.handle.spawn(async move {
            let res = match client.await {
                // One-shot requests never hang forever: a stalled server
                // must not leave a dialog or a spinner waiting indefinitely.
                Ok(c) => match limit {
                    Some(limit) => match tokio::time::timeout(limit, f(c)).await {
                        Ok(res) => res,
                        Err(_) => Err(format!("no response from the API server within {} s", limit.as_secs())),
                    },
                    None => f(c).await,
                },
                Err(e) => Err(e),
            };
            j.finish(res);
            notify.ping();
        });
        job
    }

    pub fn get_object(&self, key: &ResourceKey, ns: &str, name: &str) -> Job<ObjectData> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.job(move |c| async move { ops::get_object(&c, &key, &ns, &name).await })
    }

    pub fn replace_object(&self, key: &ResourceKey, ns: &str, name: &str, yaml: String) -> Job<ObjectData> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.mutating_job(
            Some(JOB_TIMEOUT),
            move |c| async move { ops::replace_object(&c, &key, &ns, &name, &yaml).await },
        )
    }

    pub fn apply_manifest(&self, yaml: String) -> Job<Vec<String>> {
        let discovered = self.discovered();
        // A manifest can hold hundreds of documents: each request has its
        // own time limit rather than the whole job.
        self.mutating_job(None, move |c| async move { ops::apply_manifest(&c, &discovered, &yaml).await })
    }

    pub fn delete_object(&self, key: &ResourceKey, ns: &str, name: &str, force: bool) -> Job<()> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.mutating_job(
            Some(JOB_TIMEOUT),
            move |c| async move { ops::delete_object(&c, &key, &ns, &name, force).await },
        )
    }

    pub fn scale(&self, key: &ResourceKey, ns: &str, name: &str, replicas: i32) -> Job<()> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.mutating_job(Some(JOB_TIMEOUT), move |c| async move { ops::scale(&c, &key, &ns, &name, replicas).await })
    }

    pub fn restart(&self, key: &ResourceKey, ns: &str, name: &str) -> Job<()> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.mutating_job(Some(JOB_TIMEOUT), move |c| async move { ops::restart(&c, &key, &ns, &name).await })
    }

    pub fn cordon(&self, node: &str, unschedulable: bool) -> Job<()> {
        let node = node.to_string();
        self.mutating_job(Some(JOB_TIMEOUT), move |c| async move { ops::cordon(&c, &node, unschedulable).await })
    }

    pub fn suspend_cronjob(&self, key: &ResourceKey, ns: &str, name: &str, suspend: bool) -> Job<()> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.mutating_job(Some(JOB_TIMEOUT), move |c| async move {
            ops::suspend_cronjob(&c, &key, &ns, &name, suspend).await
        })
    }

    pub fn pod_containers(&self, ns: &str, pod: &str) -> Job<Vec<ContainerInfo>> {
        let (ns, pod) = (ns.to_string(), pod.to_string());
        self.job(move |c| async move { ops::pod_containers(&c, &ns, &pod).await })
    }

    /// Ports of a pod or service that can be forwarded to.
    pub fn forwardable_ports(&self, service: bool, ns: &str, name: &str) -> Job<Vec<(String, u16)>> {
        let (ns, name) = (ns.to_string(), name.to_string());
        self.job(move |c| async move { ops::forwardable_ports(&c, service, &ns, &name).await })
    }

    /// Resolves which containers to stream for a pod or workload.
    pub fn log_targets(&self, key: &ResourceKey, ns: &str, name: &str) -> Job<Vec<LogTarget>> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.job(move |c| async move { ops::log_targets(&c, &key, &ns, &name).await })
    }

    /// One-shot filtered listing, e.g. the events of an object or the pods
    /// matching a selector.
    pub fn list(&self, key: &ResourceKey, ns: Option<&str>, query: ListQuery) -> Job<Arc<Snapshot>> {
        let (key, ns) = (key.clone(), ns.map(str::to_string));
        self.job(move |c| async move {
            watch::list_once(&c, &key, ns.as_deref(), &query).await.map(Arc::new).map_err(|f| f.message())
        })
    }

    /// Events that refer to the given object. With a `uid` only events of
    /// this incarnation match (a recreated pod of the same name has its own);
    /// node events are recorded against the node name, so pass `None` there.
    pub fn events_for(&self, ns: &str, kind: &str, name: &str, uid: Option<&str>) -> Job<Arc<Snapshot>> {
        let mut selector = match uid.filter(|u| !u.is_empty()) {
            Some(uid) => format!("involvedObject.uid={uid}"),
            None => format!("involvedObject.name={name},involvedObject.kind={kind}"),
        };
        if !ns.is_empty() {
            selector.push_str(&format!(",involvedObject.namespace={ns}"));
        }
        let query = ListQuery { field_selector: Some(selector), ..Default::default() };
        self.list(&kinds::events(), if ns.is_empty() { None } else { Some(ns) }, query)
    }

    pub fn logs(&self, targets: Vec<LogTarget>, options: LogOptions) -> Arc<LogStream> {
        let stream =
            logs::open(&self.handle, self.client(), self.client_provider(), targets, options, self.notify.clone());
        let mut open = self.logs.lock();
        open.retain(|w| w.strong_count() > 0);
        open.push(Arc::downgrade(&stream));
        stream
    }

    pub fn exec(&self, ns: &str, pod: &str, container: &str, command: Vec<String>) -> Arc<ExecSession> {
        self.exec_with(ns, pod, container, command, true)
    }

    /// Like [`Self::exec`]; with `tty` false the process gets plain pipes
    /// instead of a terminal (its input and output pass through unchanged).
    pub fn exec_with(&self, ns: &str, pod: &str, container: &str, command: Vec<String>, tty: bool) -> Arc<ExecSession> {
        // A shell can change anything: on a read-only cluster the session
        // is handed back already failed, and no request is made.
        let refused = self.refused();
        let client = self.client();
        let client = async move {
            match refused {
                Some(msg) => Err(msg),
                None => client.await,
            }
        };
        let session = exec::open(&self.handle, client, ns, pod, container, command, tty, self.notify.clone());
        let mut open = self.execs.lock();
        open.retain(|w| w.strong_count() > 0);
        open.push(Arc::downgrade(&session));
        session
    }

    /// Forwards a local port to a pod port. `local_port` of `None` picks a
    /// free one. Refused on a read-only session.
    pub fn forward_pod(&self, ns: &str, pod: &str, remote_port: u16, local_port: Option<u16>) -> Job<Arc<PortForward>> {
        let (ns, pod) = (ns.to_string(), pod.to_string());
        let (handle, notify, provider) = (self.handle.clone(), self.notify.clone(), self.client_provider());
        let label = format!("pod/{pod}");
        self.mutating_job(Some(JOB_TIMEOUT), move |c| async move {
            forward::start(handle, c, provider, ns, pod, label, remote_port, local_port, notify).await
        })
    }

    /// Forwards a local port to a service port via one of its ready pods.
    pub fn forward_service(
        &self,
        ns: &str,
        service: &str,
        port: u16,
        local_port: Option<u16>,
    ) -> Job<Arc<PortForward>> {
        let (ns, service) = (ns.to_string(), service.to_string());
        let (handle, notify, provider) = (self.handle.clone(), self.notify.clone(), self.client_provider());
        self.mutating_job(Some(JOB_TIMEOUT), move |c| async move {
            let (pod, remote) = ops::resolve_service(&c, &ns, &service, port).await?;
            let label = format!("svc/{service}");
            forward::start(handle, c, provider, ns, pod, label, remote, local_port, notify).await
        })
    }

    /// Registers a started forward so it shows up in `forwards()`.
    pub fn track_forward(&self, fwd: Arc<PortForward>) {
        self.forwards.lock().push(fwd);
    }

    pub fn forwards(&self) -> Vec<Arc<PortForward>> {
        self.forwards.lock().clone()
    }

    pub fn stop_forward(&self, id: u64) {
        let mut list = self.forwards.lock();
        if let Some(f) = list.iter().find(|f| f.id == id) {
            f.stop();
        }
        list.retain(|f| f.id != id);
    }

    /// Stops all background work for this cluster.
    pub fn shutdown(&self) {
        for t in self.tasks.lock().drain(..) {
            t.abort();
        }
        for (_, store) in self.stores.lock().drain() {
            store.stop();
        }
        for f in self.forwards.lock().drain(..) {
            f.stop();
        }
        for stream in self.logs.lock().drain(..).filter_map(|w| w.upgrade()) {
            stream.stop();
        }
        for exec in self.execs.lock().drain(..).filter_map(|w| w.upgrade()) {
            exec.close();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Owns the I/O runtime. Create one per process.
pub struct Core {
    rt: tokio::runtime::Runtime,
    notify: Notifier,
    /// Shared with every session, so replacing it reaches their next
    /// reconnect.
    sources: Arc<ArcSwap<KubeconfigSources>>,
}

impl Core {
    /// `notify` is called from background threads whenever something the UI
    /// displays may have changed.
    pub fn new(notify: Notifier) -> std::io::Result<Core> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("skiff-io")
            .enable_all()
            .build()?;
        Ok(Core { rt, notify, sources: Arc::new(ArcSwap::from_pointee(KubeconfigSources::from_env())) })
    }

    /// Replaces where kubeconfigs are read from. Takes effect for
    /// [`Self::load_kubeconfig`] at once and for every session (existing
    /// and new) on its next connection attempt; call `Session::reconnect`
    /// to apply it to a connected cluster.
    pub fn set_kubeconfig_sources(&self, sources: KubeconfigSources) {
        self.sources.store(Arc::new(sources));
    }

    pub fn kubeconfig_sources(&self) -> KubeconfigSources {
        (**self.sources.load()).clone()
    }

    /// Reads the configured kubeconfigs now (blocking file I/O; cheap).
    /// Lists exactly what [`Self::connect`] will use.
    pub fn load_kubeconfig(&self) -> LoadedKubeconfig {
        kubeconfig::load(&self.sources.load())
    }

    pub fn connect(&self, context: &str) -> Arc<Session> {
        Session::connect(self.rt.handle().clone(), self.notify.clone(), context, self.sources.clone())
    }

    pub fn handle(&self) -> Handle {
        self.rt.handle().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecState;

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !f() {
            assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A core whose kubeconfig sources point at nothing, and a session on it.
    fn offline(context: &str) -> (Core, Arc<Session>) {
        let core = Core::new(Notifier::noop()).expect("runtime");
        let missing = std::env::temp_dir().join(format!("skiff-no-such-kubeconfig-{}", std::process::id()));
        core.set_kubeconfig_sources(KubeconfigSources::explicit([missing.to_string_lossy().into_owned()]));
        let session = core.connect(context);
        (core, session)
    }

    fn refused<T>(what: &str, job: Job<T>) {
        // Refused before anything is spawned: the answer is there at once.
        match job.take() {
            Some(Err(e)) => assert_eq!(e, "prod is read-only in Skiff", "{what}"),
            Some(Ok(_)) => panic!("{what} succeeded on a read-only session"),
            None => panic!("{what} was not refused immediately"),
        }
    }

    #[test]
    fn read_only_refuses_every_mutation() {
        let (_core, s) = offline("prod");
        let deploy = ResourceKey::new("apps", "v1", "deployments", "Deployment", true);
        let cron = ResourceKey::new("batch", "v1", "cronjobs", "CronJob", true);
        assert!(!s.is_read_only());
        s.set_read_only(true);
        assert!(s.is_read_only());

        refused("replace", s.replace_object(&deploy, "ns", "web", "kind: Deployment".into()));
        refused("apply", s.apply_manifest("kind: ConfigMap".into()));
        refused("delete", s.delete_object(&deploy, "ns", "web", false));
        refused("force delete", s.delete_object(&deploy, "ns", "web", true));
        refused("scale", s.scale(&deploy, "ns", "web", 3));
        refused("restart", s.restart(&deploy, "ns", "web"));
        refused("cordon", s.cordon("node-1", true));
        refused("uncordon", s.cordon("node-1", false));
        refused("suspend", s.suspend_cronjob(&cron, "ns", "nightly", true));
        refused("forward pod", s.forward_pod("ns", "web-0", 80, None));
        refused("forward service", s.forward_service("ns", "web", 80, None));

        for exec in [
            s.exec("ns", "web-0", "app", vec!["sh".into()]),
            s.exec_with("ns", "web-0", "app", vec!["sh".into()], false),
        ] {
            wait_until("exec to be refused", || !matches!(exec.state(), ExecState::Connecting));
            match exec.state() {
                ExecState::Failed(e) => assert!(e.contains("prod is read-only in Skiff"), "{e}"),
                other => panic!("exec on a read-only session: {other:?}"),
            }
        }

        // Reads are not refused: this one fails for lack of a cluster,
        // not because of the flag.
        let get = s.get_object(&deploy, "ns", "web");
        wait_until("get to finish", || get.is_done());
        let err = get.take().expect("done").expect_err("no cluster");
        assert!(!err.contains("read-only"), "{err}");

        // The flag can be lifted at runtime.
        s.set_read_only(false);
        let scale = s.scale(&deploy, "ns", "web", 3);
        wait_until("scale to finish", || scale.is_done());
        let err = scale.take().expect("done").expect_err("no cluster");
        assert!(!err.contains("read-only"), "{err}");
    }

    #[test]
    fn missing_context_fails_with_a_structured_error() {
        let (_core, s) = offline("nowhere");
        wait_until("connection to fail", || matches!(&*s.status(), ConnStatus::Failed(_)));
        let info = s.failure().expect("failure details");
        assert_eq!(info.kind, ErrorKind::Kubeconfig);
        assert!(info.summary.contains("\"nowhere\""), "{}", info.summary);
        assert!(info.hint.is_some());
        match &*s.status() {
            ConnStatus::Failed(msg) => assert_eq!(*msg, info.to_string()),
            other => panic!("{other:?}"),
        }
        assert_eq!(s.connecting_detail(), None);
    }

    fn write_kubeconfig(tag: &str, user: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("skiff-session-{tag}-{}.yaml", std::process::id()));
        let text = format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:1\ncontexts:\n- name: ctx\n  context:\n    cluster: c\n    user: u\nusers:\n- name: u\n  user:\n{user}"
        );
        std::fs::write(&path, text).expect("write kubeconfig");
        path
    }

    #[test]
    fn missing_exec_plugin_is_named_before_anything_runs() {
        let path = write_kubeconfig(
            "noplugin",
            "    exec:\n      apiVersion: client.authentication.k8s.io/v1\n      command: skiff-test-no-such-plugin\n      installHint: Install it from example.com/plugin\n",
        );
        let core = Core::new(Notifier::noop()).expect("runtime");
        core.set_kubeconfig_sources(KubeconfigSources::explicit([path.to_string_lossy().into_owned()]));
        assert_eq!(core.load_kubeconfig().contexts.items[0].exec_command.as_deref(), Some("skiff-test-no-such-plugin"));
        let s = core.connect("ctx");
        wait_until("connection to fail", || matches!(&*s.status(), ConnStatus::Failed(_)));
        let info = s.failure().expect("failure details");
        let _ = std::fs::remove_file(&path);
        assert_eq!(info.kind, ErrorKind::ExecMissing);
        assert_eq!(info.summary, "Credential plugin `skiff-test-no-such-plugin` was not found");
        let hint = info.hint.expect("hint");
        assert!(hint.contains("PATH") && hint.ends_with("Install it from example.com/plugin"), "{hint}");
    }

    /// A slow plugin must not occupy an I/O worker, its name is shown while
    /// it runs, and its failure shows stderr but never stdout.
    #[cfg(unix)]
    #[test]
    fn exec_plugin_runs_off_the_io_workers() {
        use std::os::unix::fs::PermissionsExt;
        let script = std::env::temp_dir().join(format!("skiff-test-plugin-{}", std::process::id()));
        std::fs::write(
            &script,
            "#!/bin/sh\nsleep 1\necho 'TOKEN-ON-STDOUT'\necho 'session expired, run: aws sso login' >&2\nexit 3\n",
        )
        .expect("write plugin");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let path = write_kubeconfig(
            "slowplugin",
            &format!(
                "    exec:\n      apiVersion: client.authentication.k8s.io/v1\n      command: {}\n      interactiveMode: Never\n",
                script.display()
            ),
        );
        let core = Core::new(Notifier::noop()).expect("runtime");
        core.set_kubeconfig_sources(KubeconfigSources::explicit([path.to_string_lossy().into_owned()]));
        // More sessions than I/O workers, all waiting on the plugin.
        let sessions: Vec<_> = (0..3).map(|_| core.connect("ctx")).collect();
        let name = script.file_name().expect("name").to_string_lossy().into_owned();
        wait_until("the plugin to be shown", || {
            sessions.iter().all(|s| s.connecting_detail() == Some(format!("Waiting for {name}…")))
        });
        // The runtime still answers while all of them wait.
        let (tx, rx) = std::sync::mpsc::channel();
        core.handle().spawn(async move {
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_millis(500)).expect("I/O runtime is blocked by the credential plugin");

        for s in &sessions {
            wait_until("connection to fail", || matches!(&*s.status(), ConnStatus::Failed(_)));
            let info = s.failure().expect("failure details");
            assert_eq!(info.kind, ErrorKind::ExecFailed);
            let text = info.to_string();
            assert!(text.contains("exit status: 3") && text.contains("session expired"), "{text}");
            assert!(!text.contains("TOKEN-ON-STDOUT"), "stdout leaked: {text}");
            assert_eq!(info.hint.as_deref(), Some("Run `aws sso login` in a terminal, then reconnect."));
            assert_eq!(s.connecting_detail(), None);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&script);
    }

    #[test]
    fn sources_can_be_replaced_and_reach_the_next_reconnect() {
        let (core, s) = offline("ctx");
        wait_until("connection to fail", || matches!(&*s.status(), ConnStatus::Failed(_)));
        assert_eq!(s.failure().map(|f| f.kind), Some(ErrorKind::Kubeconfig));

        // The user adds a file that has the context: the same session
        // finds it on reconnect (and then fails to reach the cluster).
        let path = write_kubeconfig("reload", "    token: abc\n");
        let sources = core.kubeconfig_sources().with_entry(path.to_string_lossy());
        core.set_kubeconfig_sources(sources.clone());
        assert_eq!(core.kubeconfig_sources(), sources);
        let loaded = core.load_kubeconfig();
        assert_eq!(loaded.files, std::slice::from_ref(&path));
        assert_eq!(loaded.contexts.items[0].file.as_deref(), Some(path.as_path()));

        s.reconnect();
        assert_eq!(s.failure(), None, "a new attempt clears the old failure");
        wait_until("second attempt to fail", || matches!(&*s.status(), ConnStatus::Failed(_)));
        let _ = std::fs::remove_file(&path);
        let info = s.failure().expect("failure details");
        assert_eq!(info.kind, ErrorKind::Unreachable, "{info}");
        assert!(info.summary.starts_with("Cannot reach https://127.0.0.1:1"), "{}", info.summary);
    }

    #[test]
    fn a_panicking_table_task_becomes_a_failed_load() {
        let core = Core::new(Notifier::noop()).expect("runtime");
        let store = Arc::new(TableStore::new(kinds::pods(), None));
        let (tx, rx) = tokio_watch::channel::<ClientState>(None);
        let runs = Arc::new(AtomicU64::new(0));
        let counter = runs.clone();
        let task =
            core.handle().spawn(table_task(store.clone(), Notifier::noop(), rx, move |_store, _client, _notify| {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        panic!("boom in the watch code");
                    }
                    std::future::pending::<()>().await
                }
            }));
        assert_eq!(store.peek().state, LoadState::Connecting);

        let client = {
            let _guard = core.handle().enter();
            let cfg = Config::new("http://127.0.0.1:1".parse().expect("uri"));
            Client::try_from(cfg).expect("client")
        };
        tx.send_replace(Some(Ok(client.clone())));
        wait_until("the panic to surface", || store.peek().state == LoadState::Failed);
        let snap = store.peek();
        let err = snap.error.as_deref().expect("error text");
        assert!(err.contains("internal error") && err.contains("boom in the watch code"), "{err}");
        assert!(!task.is_finished(), "the task waits for a reconnect instead of dying");

        // A reconnect starts the table again.
        tx.send_replace(None);
        tx.send_replace(Some(Ok(client)));
        wait_until("the table to restart", || runs.load(Ordering::SeqCst) == 2);
        task.abort();
    }
}
