//! A connection to one cluster: client, live tables, metrics, operations.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use kube::config::{KubeConfigOptions, Kubeconfig};
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
use crate::logs::{self, LogOptions, LogStream, LogTarget};
use crate::ops::{self, ContainerInfo, ObjectData};
use crate::table::{LoadState, Snapshot, TableStore};
use crate::util::{friendly, get_request, Job, Notifier};
use crate::watch::{self, ListQuery};

/// Stores nobody has looked at for this long stop watching.
const IDLE_EVICT_SECS: i64 = 180;
/// The same, while the UI is not painting at all (hidden or minimised).
const HIDDEN_EVICT_SECS: i64 = 900;
const METRICS_INTERVAL: Duration = Duration::from_secs(15);
/// Upper bound for one-shot requests (get, apply, delete, ...).
const JOB_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextInfo {
    pub name: String,
    pub cluster: String,
    pub user: String,
    pub namespace: Option<String>,
    pub server: String,
}

#[derive(Clone, Debug, Default)]
pub struct Contexts {
    pub current: Option<String>,
    pub items: Vec<ContextInfo>,
}

/// Reads the kubeconfig (honouring `$KUBECONFIG`) and lists its contexts.
pub fn load_contexts() -> Result<Contexts, String> {
    let kc = Kubeconfig::read().map_err(|e| format!("cannot read kubeconfig: {e}"))?;
    let items = kc
        .contexts
        .iter()
        .map(|c| {
            let ctx = c.context.as_ref();
            let cluster = ctx.map(|c| c.cluster.clone()).unwrap_or_default();
            let server = kc
                .clusters
                .iter()
                .find(|n| n.name == cluster)
                .and_then(|n| n.cluster.as_ref())
                .and_then(|c| c.server.clone())
                .unwrap_or_default();
            ContextInfo {
                name: c.name.clone(),
                user: ctx.and_then(|c| c.user.clone()).unwrap_or_default(),
                namespace: ctx.and_then(|c| c.namespace.clone()),
                cluster,
                server,
            }
        })
        .collect();
    Ok(Contexts { current: kc.current_context.clone(), items })
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
}

/// Builds a client for a kubeconfig context; also returns the server URL.
async fn build_client(context: &str) -> Result<(Client, String), String> {
    let kc = Kubeconfig::read().map_err(|e| format!("cannot read kubeconfig: {e}"))?;
    let opts = KubeConfigOptions { context: Some(context.to_string()), ..Default::default() };
    let mut cfg = Config::from_custom_kubeconfig(kc, &opts)
        .await
        .map_err(|e| format!("kubeconfig context {context:?}: {e}"))?;
    // Fail fast on unreachable clusters instead of hanging the UI.
    cfg.connect_timeout = Some(Duration::from_secs(8));
    let server = cfg.cluster_url.to_string();
    let client = Client::try_from(cfg).map_err(|e| friendly(&e))?;
    Ok((client, server))
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
    let nodes: MetricsList = client
        .request(get_request("/apis/metrics.k8s.io/v1beta1/nodes", None).expect("static url"))
        .await?;
    let pods: MetricsList = client
        .request(get_request("/apis/metrics.k8s.io/v1beta1/pods", None).expect("static url"))
        .await?;
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
        m.pods
            .insert(format!("{}/{}", p.metadata.namespace, p.metadata.name).into(), total);
    }
    Ok(m)
}

impl Session {
    pub(crate) fn connect(handle: Handle, notify: Notifier, context: &str) -> Arc<Session> {
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
            let fail = |s: &Session, msg: String| {
                let _publishing = s.conn_lock.lock();
                if current(s) {
                    s.status.store(Arc::new(ConnStatus::Failed(msg.clone())));
                    s.client_tx.send_replace(Some(Err(msg)));
                    s.notify.ping();
                }
            };
            let result = build_client(&context).await;
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
                    Ok(Err(e)) => return fail(&s, format!("Cannot reach {server}: {}", friendly(&e))),
                    Err(_) => return fail(&s, format!("Cannot reach {server}: no response within 15 seconds")),
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

    /// Drops the current client and connects again (after a failure, or to
    /// pick up refreshed credentials).
    pub fn reconnect(self: &Arc<Self>) {
        {
            // Invalidate every earlier attempt before publishing anything,
            // under the lock those attempts publish under.
            let _publishing = self.conn_lock.lock();
            self.conn_gen.fetch_add(1, Ordering::SeqCst);
            self.status.store(Arc::new(ConnStatus::Connecting));
            // `send_replace` stores the value even when nobody is subscribed
            // right now; `send` would silently keep the old client.
            self.client_tx.send_replace(None);
            // Numbers from the old connection are not current any more.
            let old = self.metrics.load();
            if old.available {
                self.metrics
                    .store(Arc::new(Metrics { generation: old.generation + 1, ..Default::default() }));
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
        let mut rx = self.client_tx.subscribe();
        let (task_store, notify) = (store.clone(), self.notify.clone());
        let task = self.handle.spawn(async move {
            loop {
                let current = rx.borrow_and_update().clone();
                match current {
                    Some(Ok(client)) => {
                        tokio::select! {
                            _ = watch::run(task_store.clone(), client, notify.clone()) => return,
                            changed = rx.changed() => if changed.is_err() { return },
                        }
                    }
                    other => {
                        let mut snap = Snapshot::empty(if other.is_some() {
                            LoadState::Failed
                        } else {
                            LoadState::Connecting
                        });
                        if let Some(Err(e)) = other {
                            snap.error = Some(e.into());
                        }
                        snap.generation = task_store.peek().generation + 1;
                        task_store.publish(snap);
                        notify.ping();
                        if rx.changed().await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
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
        let job = Job::new();
        let (j, notify, client) = (job.clone(), self.notify.clone(), self.client());
        self.handle.spawn(async move {
            let res = match client.await {
                // One-shot requests never hang forever: a stalled server
                // must not leave a dialog or a spinner waiting indefinitely.
                Ok(c) => match tokio::time::timeout(JOB_TIMEOUT, f(c)).await {
                    Ok(res) => res,
                    Err(_) => Err(format!("no response from the API server within {} s", JOB_TIMEOUT.as_secs())),
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
        self.job(move |c| async move { ops::replace_object(&c, &key, &ns, &name, &yaml).await })
    }

    pub fn apply_manifest(&self, yaml: String) -> Job<Vec<String>> {
        let discovered = self.discovered();
        self.job(move |c| async move { ops::apply_manifest(&c, &discovered, &yaml).await })
    }

    pub fn delete_object(&self, key: &ResourceKey, ns: &str, name: &str, force: bool) -> Job<()> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.job(move |c| async move { ops::delete_object(&c, &key, &ns, &name, force).await })
    }

    pub fn scale(&self, key: &ResourceKey, ns: &str, name: &str, replicas: i32) -> Job<()> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.job(move |c| async move { ops::scale(&c, &key, &ns, &name, replicas).await })
    }

    pub fn restart(&self, key: &ResourceKey, ns: &str, name: &str) -> Job<()> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.job(move |c| async move { ops::restart(&c, &key, &ns, &name).await })
    }

    pub fn cordon(&self, node: &str, unschedulable: bool) -> Job<()> {
        let node = node.to_string();
        self.job(move |c| async move { ops::cordon(&c, &node, unschedulable).await })
    }

    pub fn suspend_cronjob(&self, key: &ResourceKey, ns: &str, name: &str, suspend: bool) -> Job<()> {
        let (key, ns, name) = (key.clone(), ns.to_string(), name.to_string());
        self.job(move |c| async move { ops::suspend_cronjob(&c, &key, &ns, &name, suspend).await })
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
            watch::list_once(&c, &key, ns.as_deref(), &query)
                .await
                .map(Arc::new)
                .map_err(|f| f.message())
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
        let stream = logs::open(
            &self.handle,
            self.client(),
            self.client_provider(),
            targets,
            options,
            self.notify.clone(),
        );
        let mut open = self.logs.lock();
        open.retain(|w| w.strong_count() > 0);
        open.push(Arc::downgrade(&stream));
        stream
    }

    pub fn exec(&self, ns: &str, pod: &str, container: &str, command: Vec<String>) -> Arc<ExecSession> {
        let session = exec::open(&self.handle, self.client(), ns, pod, container, command, self.notify.clone());
        let mut open = self.execs.lock();
        open.retain(|w| w.strong_count() > 0);
        open.push(Arc::downgrade(&session));
        session
    }

    /// Forwards a local port to a pod port. `local_port` of `None` picks a
    /// free one.
    pub fn forward_pod(&self, ns: &str, pod: &str, remote_port: u16, local_port: Option<u16>) -> Job<Arc<PortForward>> {
        let (ns, pod) = (ns.to_string(), pod.to_string());
        let (handle, notify, provider) = (self.handle.clone(), self.notify.clone(), self.client_provider());
        let label = format!("pod/{pod}");
        self.job(move |c| async move {
            forward::start(handle, c, provider, ns, pod, label, remote_port, local_port, notify).await
        })
    }

    /// Forwards a local port to a service port via one of its ready pods.
    pub fn forward_service(&self, ns: &str, service: &str, port: u16, local_port: Option<u16>) -> Job<Arc<PortForward>> {
        let (ns, service) = (ns.to_string(), service.to_string());
        let (handle, notify, provider) = (self.handle.clone(), self.notify.clone(), self.client_provider());
        self.job(move |c| async move {
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
        Ok(Core { rt, notify })
    }

    pub fn connect(&self, context: &str) -> Arc<Session> {
        Session::connect(self.rt.handle().clone(), self.notify.clone(), context)
    }

    pub fn handle(&self) -> Handle {
        self.rt.handle().clone()
    }
}
