//! List + watch engine built on the server-side Table API.
//!
//! The API server renders the same columns `kubectl get` shows (including
//! CRD printer columns) and ships only object metadata alongside, so a view
//! costs a fraction of the bandwidth and parsing of full objects, and every
//! resource type works without client-side knowledge of its schema.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::AsyncBufReadExt;
use indexmap::IndexMap;
use kube::Client;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::Value;

use crate::cells::{cell_from_value, is_status_column, parse_time, value_text, Cell, ColKind, Tone};
use crate::kinds::ResourceKey;
use crate::table::{ColSource, Column, LoadState, Row, Snapshot, TableStore};
use crate::util::{enc, get_request, Notifier};
use crate::views::{tweaks_for, Tweaks};

pub(crate) const TABLE_ACCEPT: &str = "application/json;as=Table;v=v1;g=meta.k8s.io,application/json";
/// A small first page paints quickly; later pages are large to keep the
/// number of round trips low on high-latency links.
const FIRST_PAGE: usize = 250;
const PAGE: usize = 2000;
const MIN_PUBLISH: Duration = Duration::from_millis(80);
/// The server ends each watch after this long and we resume. Kept short so
/// that silence always has a tight upper bound (see `WATCH_SILENCE`).
const WATCH_TIMEOUT_SECS: u32 = 120;
const POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Longest we wait for the response headers of a list page or watch. Short,
/// so a request sent down a dead pooled connection is abandoned quickly.
const HEADER_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest a list page body may take to arrive once it has started.
const BODY_TIMEOUT: Duration = Duration::from_secs(60);
/// A healthy watch is never silent for longer than `WATCH_TIMEOUT_SECS`
/// (the server ends it then, and usually sends bookmarks before that), so
/// more silence than this means the connection died without telling us.
const WATCH_SILENCE: Duration = Duration::from_secs(WATCH_TIMEOUT_SECS as u64 + 30);
/// After watches keep ending immediately we poll, and try watching again
/// this much later.
const WATCH_RETRY_AFTER: Duration = Duration::from_secs(300);
const BACKOFF_START: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const PERMANENT_RETRY: Duration = Duration::from_secs(60);

#[derive(Deserialize, Default)]
struct ListMetaLite {
    #[serde(default, rename = "resourceVersion")]
    resource_version: Option<String>,
    #[serde(default, rename = "continue")]
    cont: Option<String>,
}

#[derive(Deserialize)]
struct ColDef {
    name: String,
    #[serde(default, rename = "type")]
    ty: String,
    #[serde(default)]
    priority: i32,
}

#[derive(Deserialize)]
struct RowRaw<'a> {
    #[serde(default)]
    cells: Vec<Value>,
    #[serde(default, borrow)]
    object: Option<&'a RawValue>,
}

/// A Table, a plain list, or a Status — whichever the server sent.
#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    metadata: ListMetaLite,
    #[serde(default, rename = "columnDefinitions")]
    column_definitions: Option<Vec<ColDef>>,
    #[serde(default, borrow)]
    rows: Option<Vec<RowRaw<'a>>>,
    #[serde(default, borrow)]
    items: Option<Vec<&'a RawValue>>,
    #[serde(default)]
    code: Option<u16>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct WatchLine<'a> {
    #[serde(rename = "type")]
    ty: String,
    #[serde(borrow)]
    object: Envelope<'a>,
}

#[derive(Deserialize, Default)]
struct ObjLite<'a> {
    #[serde(default, borrow)]
    metadata: MetaLite<'a>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MetaLite<'a> {
    #[serde(default, borrow)]
    name: Cow<'a, str>,
    #[serde(default, borrow)]
    namespace: Cow<'a, str>,
    #[serde(default, borrow)]
    uid: Cow<'a, str>,
    #[serde(default, borrow)]
    resource_version: Cow<'a, str>,
    #[serde(default, borrow)]
    creation_timestamp: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    deletion_timestamp: Option<Cow<'a, str>>,
    #[serde(default)]
    labels: Option<BTreeMap<String, String>>,
    #[serde(default)]
    owner_references: Option<Vec<OwnerLite>>,
}

#[derive(Deserialize)]
struct OwnerLite {
    kind: String,
    name: String,
    #[serde(default)]
    controller: Option<bool>,
}

/// How server cells map onto our row cells for one resource type.
pub(crate) struct Plan {
    pub columns: Arc<[Column]>,
    /// server cell index -> (row cell index, interpretation)
    map: Vec<Option<(usize, ColKind)>>,
    name_idx: Option<usize>,
    n_cells: usize,
    /// (title, row cell index) for enricher lookups.
    by_title: Vec<(Box<str>, usize)>,
    /// Row cell indices that carry status semantics.
    status_cells: Vec<usize>,
    tweaks: Tweaks,
    pub default_sort: Option<(usize, bool)>,
}

/// Server columns whose text is a rendered duration such as "48m" (which
/// would otherwise be read as the quantity 0.048).
fn is_duration_column(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "duration" || n == "last schedule" || n == "last seen" || n == "first seen" || n.ends_with(" age")
}

fn has(list: &[&str], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}

impl Plan {
    fn build(key: &ResourceKey, defs: &[ColDef]) -> Plan {
        let tweaks = tweaks_for(key);
        let mut columns = Vec::with_capacity(defs.len() + 4);
        let mut map = vec![None; defs.len()];
        let mut by_title = Vec::new();
        let mut status_cells = Vec::new();
        let mut name_idx = None;
        let mut n_cells = 0;

        columns.push(Column {
            title: "Name".into(),
            source: ColSource::Name,
            kind: ColKind::Text,
            visible: !tweaks.hide_name,
        });
        if key.namespaced {
            columns.push(Column {
                title: "Namespace".into(),
                source: ColSource::Namespace,
                kind: ColKind::Text,
                visible: true,
            });
        }
        for (i, def) in defs.iter().enumerate() {
            if def.name.eq_ignore_ascii_case("name") {
                name_idx = Some(i);
                continue;
            }
            // "Age" and "Created At" both restate metadata.creationTimestamp,
            // which the Age column shows live.
            if def.name.eq_ignore_ascii_case("age") || def.name.eq_ignore_ascii_case("created at") {
                continue;
            }
            let kind = if def.ty == "date" || has(tweaks.time_cols, &def.name) || is_duration_column(&def.name) {
                ColKind::Time
            } else if def.ty == "integer" || def.ty == "number" {
                ColKind::Number
            } else if is_status_column(&def.name) {
                ColKind::Status
            } else {
                ColKind::Text
            };
            let visible = if def.priority == 0 {
                !has(tweaks.hide, &def.name)
            } else {
                has(tweaks.show_wide, &def.name)
            };
            map[i] = Some((n_cells, kind));
            if kind == ColKind::Status {
                status_cells.push(n_cells);
            }
            by_title.push((def.name.clone().into_boxed_str(), n_cells));
            columns.push(Column {
                title: def.name.clone().into(),
                source: ColSource::Cell(n_cells),
                kind,
                visible,
            });
            n_cells += 1;
        }
        for (title, kind) in tweaks.extra {
            // A newer server may already render this column: reuse it.
            if let Some(existing) = columns.iter_mut().find(|c: &&mut Column| c.title.eq_ignore_ascii_case(title)) {
                existing.visible = true;
                continue;
            }
            by_title.push(((*title).into(), n_cells));
            columns.push(Column {
                title: (*title).into(),
                source: ColSource::Cell(n_cells),
                kind: *kind,
                visible: true,
            });
            n_cells += 1;
        }
        if tweaks.owner_col {
            columns.push(Column {
                title: "Controlled By".into(),
                source: ColSource::Owner,
                kind: ColKind::Text,
                visible: true,
            });
        }
        columns.push(Column {
            title: "Age".into(),
            source: ColSource::Age,
            kind: ColKind::Time,
            visible: true,
        });
        let default_sort = tweaks
            .default_sort
            .and_then(|(title, desc)| columns.iter().position(|c| c.title.eq_ignore_ascii_case(title)).map(|i| (i, desc)));
        Plan {
            columns: columns.into(),
            map,
            name_idx,
            n_cells,
            by_title,
            status_cells,
            tweaks,
            default_sort,
        }
    }

    fn build_row(&self, cells_raw: &[Value], object: Option<&RawValue>) -> Option<Arc<Row>> {
        let meta = object
            .and_then(|o| serde_json::from_str::<ObjLite>(o.get()).ok())
            .unwrap_or_default()
            .metadata;
        let name: Box<str> = if !meta.name.is_empty() {
            meta.name.as_ref().into()
        } else {
            self.name_idx.and_then(|i| cells_raw.get(i)).map(value_text)?.into()
        };
        if name.is_empty() {
            return None;
        }
        let namespace: Box<str> = meta.namespace.as_ref().into();
        let created = meta.creation_timestamp.as_deref().map(parse_time).unwrap_or(0);

        let mut cells = vec![Cell::empty(); self.n_cells];
        for (i, v) in cells_raw.iter().enumerate() {
            if let Some(Some((dst, kind))) = self.map.get(i) {
                cells[*dst] = cell_from_value(v, *kind);
            }
        }
        if let (Some(enrich), Some(obj)) = (self.tweaks.enrich, object) {
            let by_title = &self.by_title;
            let mut set = |title: &str, cell: Cell| {
                if let Some((_, idx)) = by_title.iter().find(|(t, _)| t.eq_ignore_ascii_case(title)) {
                    cells[*idx] = cell;
                }
            };
            enrich(obj, created, &mut set);
        }

        // A finished object ("Completed") legitimately has 0/1 ready.
        let done = self.status_cells.iter().any(|i| cells[*i].tone == Tone::Muted && !cells[*i].text.is_empty());
        let mut tone = Tone::Normal;
        for i in &self.status_cells {
            let c = &mut cells[*i];
            if done && c.tone == Tone::Warn && c.text.contains('/') {
                c.tone = Tone::Normal;
            }
            if c.tone >= Tone::Warn {
                tone = tone.max(c.tone);
            }
        }
        let deleting = meta.deletion_timestamp.is_some();

        let owner = meta.owner_references.and_then(|refs| {
            let pick = refs.iter().position(|r| r.controller == Some(true)).unwrap_or(0);
            refs.into_iter().nth(pick).map(|r| Box::new((r.kind.into_boxed_str(), r.name.into_boxed_str())))
        });

        let mut labels = String::new();
        if let Some(map) = &meta.labels {
            for (k, v) in map {
                if !labels.is_empty() {
                    labels.push(' ');
                }
                labels.push_str(k);
                labels.push('=');
                labels.push_str(v);
            }
        }

        let mut search = String::with_capacity(name.len() + namespace.len() + labels.len() + 64);
        search.push_str(&name);
        search.push(' ');
        search.push_str(&namespace);
        for c in &cells {
            if !c.text.is_empty() {
                search.push(' ');
                search.push_str(&c.text);
            }
        }
        if let Some(o) = &owner {
            search.push(' ');
            search.push_str(&o.1);
        }
        if !labels.is_empty() {
            search.push(' ');
            search.push_str(&labels);
        }
        search.make_ascii_lowercase();

        let uid: Box<str> = if meta.uid.is_empty() {
            format!("{namespace}/{name}").into()
        } else {
            meta.uid.as_ref().into()
        };
        Some(Arc::new(Row {
            uid,
            name,
            namespace,
            created,
            deleting,
            tone,
            rv: meta.resource_version.as_ref().into(),
            cells: cells.into_boxed_slice(),
            owner,
            labels: labels.into_boxed_str(),
            search: search.into_boxed_str(),
        }))
    }
}

#[derive(Debug)]
pub(crate) enum Fail {
    /// Resource version too old: list again.
    Gone,
    /// The user asked for a refresh mid-list: start over.
    Restart,
    /// Won't fix itself (forbidden, not found, ...).
    Permanent { code: u16, msg: String },
    Transient(String),
}

impl Fail {
    pub fn message(&self) -> String {
        match self {
            Fail::Gone => "resource version expired".into(),
            Fail::Restart => "restarting".into(),
            Fail::Permanent { msg, .. } | Fail::Transient(msg) => msg.clone(),
        }
    }
}

impl From<kube::Error> for Fail {
    fn from(e: kube::Error) -> Self {
        match e {
            kube::Error::Api(status) => {
                let msg = if status.message.is_empty() { status.reason.clone() } else { status.message.clone() };
                match status.code {
                    410 => Fail::Gone,
                    400 | 401 | 403 | 404 | 405 | 406 | 415 | 422 => Fail::Permanent { code: status.code, msg },
                    _ => Fail::Transient(msg),
                }
            }
            kube::Error::SerdeError(e) => Fail::Permanent { code: 0, msg: format!("unexpected response: {e}") },
            other => Fail::Transient(crate::util::friendly(&other)),
        }
    }
}

/// Extra server-side filters for a listing.
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub label_selector: Option<String>,
    pub field_selector: Option<String>,
}

impl ListQuery {
    fn append(&self, url: &mut String) {
        if let Some(l) = &self.label_selector {
            url.push_str("&labelSelector=");
            url.push_str(&enc(l));
        }
        if let Some(f) = &self.field_selector {
            url.push_str("&fieldSelector=");
            url.push_str(&enc(f));
        }
    }
}

struct Page {
    rows: Vec<Arc<Row>>,
    cont: Option<String>,
    rv: String,
    is_table: bool,
}

async fn fetch_page(
    client: &Client,
    key: &ResourceKey,
    scope: Option<&str>,
    query: &ListQuery,
    cont: Option<&str>,
    plan: &mut Option<Arc<Plan>>,
    limit: usize,
) -> Result<Page, Fail> {
    let include_object = tweaks_for(key).include_object;
    let mut url = format!("{}?limit={}", key.list_path(scope), limit);
    if include_object {
        url.push_str("&includeObject=Object");
    }
    query.append(&mut url);
    if let Some(c) = cont {
        url.push_str("&continue=");
        url.push_str(&enc(c));
    }
    let req = get_request(&url, Some(TABLE_ACCEPT)).map_err(Fail::Transient)?;
    let res = tokio::time::timeout(HEADER_TIMEOUT, client.send(req.map(kube::client::Body::from)))
        .await
        .map_err(|_| Fail::Transient("no response from the API server".into()))??;
    let status = res.status();
    let bytes = tokio::time::timeout(BODY_TIMEOUT, res.into_body().collect_bytes())
        .await
        .map_err(|_| Fail::Transient("timed out reading the response".into()))??;
    if !status.is_success() {
        // The body is a Status object describing the failure.
        return Err(match serde_json::from_slice::<kube::core::Status>(&bytes) {
            Ok(st) => kube::Error::Api(Box::new(st)).into(),
            Err(_) => {
                let msg = format!("HTTP {}", status.as_u16());
                match status.as_u16() {
                    410 => Fail::Gone,
                    400 | 401 | 403 | 404 | 405 | 406 | 415 | 422 => Fail::Permanent { code: status.as_u16(), msg },
                    _ => Fail::Transient(msg),
                }
            }
        });
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| Fail::Permanent { code: 0, msg: format!("unexpected response: {e}") })?;
    let env: Envelope = serde_json::from_str(text)
        .map_err(|e| Fail::Permanent { code: 0, msg: format!("unexpected response: {e}") })?;
    let is_table = env.kind == "Table";
    if plan.is_none() {
        let defs = env.column_definitions.as_deref().unwrap_or(&[]);
        *plan = Some(Arc::new(Plan::build(key, defs)));
    }
    let p = plan.as_ref().unwrap();
    let mut rows = Vec::new();
    if let Some(raw_rows) = &env.rows {
        rows.reserve(raw_rows.len());
        for r in raw_rows {
            if let Some(row) = p.build_row(&r.cells, r.object) {
                rows.push(row);
            }
        }
    } else if let Some(items) = &env.items {
        for item in items {
            if let Some(row) = p.build_row(&[], Some(item)) {
                rows.push(row);
            }
        }
    }
    Ok(Page {
        rows,
        cont: env.metadata.cont.filter(|c| !c.is_empty()),
        rv: env.metadata.resource_version.unwrap_or_default(),
        is_table,
    })
}

/// One-shot listing (no watch), e.g. the pods of a deployment.
pub(crate) async fn list_once(
    client: &Client,
    key: &ResourceKey,
    scope: Option<&str>,
    query: &ListQuery,
) -> Result<Snapshot, Fail> {
    let mut plan = None;
    let mut rows = Vec::new();
    let mut cont: Option<String> = None;
    loop {
        let page = fetch_page(client, key, scope, query, cont.as_deref(), &mut plan, PAGE).await?;
        rows.extend(page.rows);
        cont = page.cont;
        if cont.is_none() || rows.len() >= 5000 {
            break;
        }
    }
    let plan = plan.unwrap();
    Ok(Snapshot {
        generation: 1,
        columns: plan.columns.clone(),
        rows,
        state: LoadState::Live,
        error: None,
        error_code: 0,
        default_sort: plan.default_sort,
    })
}

struct Engine {
    store: Arc<TableStore>,
    client: Client,
    notify: Notifier,
    rows: IndexMap<Box<str>, Arc<Row>>,
    plan: Option<Arc<Plan>>,
    generation: u64,
    state: LoadState,
    error: Option<Box<str>>,
    error_code: u16,
    /// `TableStore::refresh_gen` value the current list accounts for.
    handled_refresh: u64,
    /// A complete list has succeeded at least once.
    listed_once: bool,
    /// Watching does not work here (not permitted, or something between us
    /// and the server cuts watches short): re-list periodically.
    poll_only: bool,
    /// When to give watching another try while `poll_only`.
    watch_retry_at: Option<Instant>,
}

enum WatchEnd {
    /// Server closed the stream (normal timeout); resume from `rv`.
    /// `productive` is false when it ended almost immediately with nothing.
    Resume { productive: bool },
    Relist,
    Failed(Fail),
}

impl Engine {
    fn publish(&mut self) {
        self.generation += 1;
        let (columns, default_sort) = match &self.plan {
            Some(p) => (p.columns.clone(), p.default_sort),
            None => (Arc::from(Vec::new()), None),
        };
        self.store.publish(Snapshot {
            generation: self.generation,
            columns,
            rows: self.rows.values().cloned().collect(),
            state: self.state,
            error: self.error.clone(),
            error_code: self.error_code,
            default_sort,
        });
        self.notify.ping();
    }

    fn set_state(&mut self, state: LoadState, fail: Option<&Fail>) {
        self.state = state;
        match fail {
            Some(f) => {
                self.error = Some(f.message().into());
                self.error_code = match f {
                    Fail::Permanent { code, .. } => *code,
                    _ => 0,
                };
            }
            None => {
                self.error = None;
                self.error_code = 0;
            }
        }
    }

    /// Whether the user asked for a refresh since the current list began.
    fn refresh_requested(&self) -> bool {
        self.store.refresh_gen.load(Ordering::SeqCst) != self.handled_refresh
    }

    /// Lists everything. Returns (resourceVersion, server speaks Table).
    async fn list(&mut self) -> Result<(String, bool), Fail> {
        self.handled_refresh = self.store.refresh_gen.load(Ordering::SeqCst);
        let scope = self.store.scope.clone();
        let key = self.store.key.clone();
        let initial = self.rows.is_empty();
        let mut fresh: IndexMap<Box<str>, Arc<Row>> = IndexMap::new();
        let mut plan: Option<Arc<Plan>> = None;
        let mut cont: Option<String> = None;
        let mut is_table;
        let rv;
        let query = ListQuery::default();
        loop {
            let limit = if cont.is_none() && initial { FIRST_PAGE } else { PAGE };
            let page = loop {
                let fetch = fetch_page(
                    &self.client,
                    &key,
                    scope.as_deref(),
                    &query,
                    cont.as_deref(),
                    &mut plan,
                    limit,
                );
                tokio::select! {
                    res = fetch => break res?,
                    _ = self.store.relist.notified() => {
                        if self.refresh_requested() {
                            return Err(Fail::Restart);
                        }
                        // A stale wake-up: ask for the same page again.
                    }
                }
            };
            is_table = page.is_table;
            for row in page.rows {
                fresh.insert(row.uid.clone(), row);
            }
            if page.cont.is_none() {
                rv = page.rv;
                break;
            }
            cont = page.cont;
            if initial {
                // Show rows as pages arrive instead of a long blank wait.
                self.plan = plan.clone();
                self.rows = fresh.clone();
                self.set_state(LoadState::Loading, None);
                self.publish();
            }
        }
        self.plan = plan;
        self.rows = fresh;
        self.listed_once = true;
        Ok((rv, is_table))
    }

    /// Applies one watch line. Returns (rows changed, was a bookmark).
    fn apply(&mut self, line: &[u8], rv: &mut String) -> Result<(bool, bool), Fail> {
        let ev: WatchLine = match serde_json::from_slice(line) {
            Ok(ev) => ev,
            Err(e) => {
                tracing::debug!("unparseable watch line: {e}");
                return Ok((false, false));
            }
        };
        let plan = match &self.plan {
            Some(p) => p.clone(),
            None => return Ok((false, false)),
        };
        let env = ev.object;
        match ev.ty.as_str() {
            "ERROR" => {
                let code = env.code.unwrap_or(0);
                let msg = env.message.unwrap_or_else(|| "watch error".into());
                Err(if code == 410 { Fail::Gone } else { Fail::Transient(msg) })
            }
            "BOOKMARK" => {
                let from_row = env
                    .rows
                    .as_ref()
                    .and_then(|r| r.first())
                    .and_then(|r| r.object)
                    .and_then(|o| serde_json::from_str::<ObjLite>(o.get()).ok())
                    .map(|o| o.metadata.resource_version.into_owned())
                    .filter(|v| !v.is_empty());
                if let Some(v) = from_row.or(env.metadata.resource_version.filter(|v| !v.is_empty())) {
                    *rv = v;
                }
                Ok((false, true))
            }
            ty @ ("ADDED" | "MODIFIED" | "DELETED") => {
                let mut changed = false;
                for r in env.rows.as_deref().unwrap_or(&[]) {
                    let Some(row) = plan.build_row(&r.cells, r.object) else { continue };
                    if !row.rv.is_empty() {
                        *rv = row.rv.to_string();
                    }
                    if ty == "DELETED" {
                        changed |= self.rows.swap_remove(&row.uid).is_some();
                    } else {
                        self.rows.insert(row.uid.clone(), row);
                        changed = true;
                    }
                }
                Ok((changed, false))
            }
            _ => Ok((false, false)),
        }
    }

    async fn watch(&mut self, rv: &mut String) -> WatchEnd {
        let key = &self.store.key;
        let mut url = format!(
            "{}?watch=1&allowWatchBookmarks=true&timeoutSeconds={}&resourceVersion={}",
            key.list_path(self.store.scope.as_deref()),
            WATCH_TIMEOUT_SECS,
            enc(rv)
        );
        if tweaks_for(key).include_object {
            url.push_str("&includeObject=Object");
        }
        let req = match get_request(&url, Some(TABLE_ACCEPT)) {
            Ok(r) => r,
            Err(e) => return WatchEnd::Failed(Fail::Transient(e)),
        };
        let started = Instant::now();
        let reader = match tokio::time::timeout(HEADER_TIMEOUT, self.client.request_stream(req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return WatchEnd::Failed(e.into()),
            Err(_) => return WatchEnd::Failed(Fail::Transient("timed out opening the watch".into())),
        };
        futures::pin_mut!(reader);
        if self.state != LoadState::Live {
            self.set_state(LoadState::Live, None);
            self.publish();
        }

        let mut buf: Vec<u8> = Vec::with_capacity(4096);
        let mut dirty = false;
        let mut last_publish = Instant::now();
        // Liveness: a healthy watch delivers events or periodic bookmarks.
        // Total silence means the connection died without telling us (sleep,
        // VPN drop, NAT timeout), which no read would ever report.
        let mut last_data = Instant::now();
        let mut lines = 0u64;
        let store = self.store.clone();
        loop {
            let until_stalled = WATCH_SILENCE.saturating_sub(last_data.elapsed());
            if until_stalled.is_zero() {
                if dirty {
                    self.publish();
                }
                return WatchEnd::Failed(Fail::Transient("connection stalled".into()));
            }
            let wait = if dirty {
                MIN_PUBLISH.saturating_sub(last_publish.elapsed()).min(until_stalled)
            } else {
                until_stalled
            };
            // `read_until` keeps partial data in `buf` when cancelled, so
            // timing out here loses nothing.
            let read = tokio::select! {
                r = tokio::time::timeout(wait, reader.read_until(b'\n', &mut buf)) => r,
                _ = store.relist.notified() => {
                    if self.refresh_requested() {
                        if dirty {
                            self.publish();
                        }
                        return WatchEnd::Relist;
                    }
                    continue;
                }
            };
            match read {
                Err(_elapsed) => {
                    if dirty {
                        self.publish();
                        dirty = false;
                        last_publish = Instant::now();
                    }
                }
                Ok(Err(e)) => {
                    if dirty {
                        self.publish();
                    }
                    return WatchEnd::Failed(Fail::Transient(e.to_string()));
                }
                Ok(Ok(_)) => {
                    if buf.last() != Some(&b'\n') {
                        // EOF: the server ended the watch.
                        if dirty {
                            self.publish();
                        }
                        let productive = lines > 0 || started.elapsed() > Duration::from_secs(5);
                        return WatchEnd::Resume { productive };
                    }
                    last_data = Instant::now();
                    lines += 1;
                    let res = self.apply(&buf, rv);
                    buf.clear();
                    match res {
                        Ok((changed, _bookmark)) => dirty |= changed,
                        // This is how the API server reports an expired
                        // resourceVersion. Report it as a failure so that
                        // the caller backs off if it keeps happening.
                        Err(f) => {
                            if dirty {
                                self.publish();
                            }
                            return WatchEnd::Failed(f);
                        }
                    }
                    if dirty && last_publish.elapsed() >= MIN_PUBLISH {
                        self.publish();
                        dirty = false;
                        last_publish = Instant::now();
                    }
                }
            }
        }
    }

    async fn pause(&self, d: Duration) {
        tokio::select! {
            _ = tokio::time::sleep(d) => {}
            _ = self.store.relist.notified() => {}
        }
    }

    async fn run(mut self) {
        let mut backoff = BACKOFF_START;
        loop {
            // Loading is shown for the first list and for a refresh of
            // existing rows (which stay visible meanwhile). Routine re-lists
            // of a polled table and retries after a failure keep their
            // state, so the UI does not flicker between messages.
            if !matches!(self.state, LoadState::Polling | LoadState::Reconnecting | LoadState::Failed) {
                self.set_state(LoadState::Loading, None);
                self.publish();
            }
            if self.poll_only && self.watch_retry_at.is_some_and(|t| Instant::now() >= t) {
                self.poll_only = false;
                self.watch_retry_at = None;
            }
            let (mut rv, is_table) = match self.list().await {
                Ok(v) => v,
                Err(Fail::Restart) => continue,
                Err(f) => {
                    if !self.listed_once {
                        // A partial first list is not worth showing as data.
                        self.rows.clear();
                    }
                    let permanent = matches!(f, Fail::Permanent { .. });
                    let state = if self.rows.is_empty() || permanent { LoadState::Failed } else { LoadState::Reconnecting };
                    if permanent {
                        self.rows.clear();
                    }
                    self.set_state(state, Some(&f));
                    self.publish();
                    self.pause(if permanent { PERMANENT_RETRY } else { backoff }).await;
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                    continue;
                }
            };

            if !is_table || rv.is_empty() || self.poll_only {
                // No Table/watch support (some aggregated APIs), or watching
                // is not permitted: poll.
                backoff = BACKOFF_START;
                self.set_state(LoadState::Polling, None);
                self.publish();
                self.pause(POLL_INTERVAL).await;
                continue;
            }
            self.set_state(LoadState::Live, None);
            self.publish();

            let mut failures = 0u32;
            let mut unproductive = 0u32;
            loop {
                let started = Instant::now();
                match self.watch(&mut rv).await {
                    WatchEnd::Resume { productive: true } => {
                        failures = 0;
                        unproductive = 0;
                        backoff = BACKOFF_START;
                    }
                    WatchEnd::Resume { productive: false } => {
                        // Something between us and the API server ends
                        // watches immediately. Don't spin, and don't claim
                        // to be live either: fall back to polling.
                        unproductive += 1;
                        if unproductive >= 3 {
                            self.poll_only = true;
                            self.watch_retry_at = Some(Instant::now() + WATCH_RETRY_AFTER);
                            break;
                        }
                        self.pause(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                        if self.refresh_requested() {
                            break;
                        }
                    }
                    WatchEnd::Relist => break,
                    WatchEnd::Failed(Fail::Gone) | WatchEnd::Failed(Fail::Restart) => {
                        // Usually immediate; back off if it keeps happening.
                        if started.elapsed() < Duration::from_secs(5) {
                            self.pause(backoff).await;
                            backoff = (backoff * 2).min(BACKOFF_MAX);
                        }
                        break;
                    }
                    WatchEnd::Failed(Fail::Permanent { code: 403 | 405, .. }) => {
                        self.poll_only = true;
                        break;
                    }
                    WatchEnd::Failed(f @ Fail::Permanent { .. }) => {
                        self.set_state(LoadState::Failed, Some(&f));
                        self.publish();
                        self.pause(PERMANENT_RETRY).await;
                        break;
                    }
                    WatchEnd::Failed(f) => {
                        if started.elapsed() > Duration::from_secs(30) {
                            failures = 0;
                            backoff = BACKOFF_START;
                        }
                        failures += 1;
                        self.set_state(LoadState::Reconnecting, Some(&f));
                        self.publish();
                        self.pause(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                        // A refresh asked for during the pause must not be
                        // lost to another watch attempt.
                        if failures >= 3 || self.refresh_requested() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// Drives a store until aborted.
pub(crate) async fn run(store: Arc<TableStore>, client: Client, notify: Notifier) {
    Engine {
        store,
        client,
        notify,
        rows: IndexMap::new(),
        plan: None,
        generation: 0,
        state: LoadState::Loading,
        error: None,
        error_code: 0,
        handled_refresh: 0,
        listed_once: false,
        poll_only: false,
        watch_retry_at: None,
    }
    .run()
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds;

    const POD_TABLE: &str = r#"{
      "kind":"Table","apiVersion":"meta.k8s.io/v1",
      "metadata":{"resourceVersion":"1234","continue":""},
      "columnDefinitions":[
        {"name":"Name","type":"string","format":"name","priority":0},
        {"name":"Ready","type":"string","priority":0},
        {"name":"Status","type":"string","priority":0},
        {"name":"Restarts","type":"string","priority":0},
        {"name":"Age","type":"string","priority":0},
        {"name":"IP","type":"string","priority":1},
        {"name":"Node","type":"string","priority":1},
        {"name":"Nominated Node","type":"string","priority":1}
      ],
      "rows":[
        {"cells":["web-1","1/1","Running","0","5m","10.0.0.5","node-a","<none>"],
         "object":{"kind":"PartialObjectMetadata","metadata":{"name":"web-1","namespace":"shop","uid":"u1","resourceVersion":"1200",
           "creationTimestamp":"2024-01-01T00:00:00Z","labels":{"app":"web","tier":"fe"},
           "ownerReferences":[{"kind":"ReplicaSet","name":"web-abc","controller":true}],
           "managedFields":[{"manager":"x","fieldsV1":{"f:a":{}}}]}}},
        {"cells":["job-x","0/1","Completed","0","5m","10.0.0.6","node-a","<none>"],
         "object":{"kind":"PartialObjectMetadata","metadata":{"name":"job-x","namespace":"shop","uid":"u2","resourceVersion":"1201"}}},
        {"cells":["bad","0/1","CrashLoopBackOff","12 (3m ago)","5m","10.0.0.7","node-b","<none>"],
         "object":{"kind":"PartialObjectMetadata","metadata":{"name":"bad","namespace":"shop","uid":"u3","resourceVersion":"1202","deletionTimestamp":"2024-01-01T00:00:00Z"}}}
      ]}"#;

    fn parse(text: &str, key: &ResourceKey) -> (Arc<Plan>, Vec<Arc<Row>>) {
        let env: Envelope = serde_json::from_str(text).unwrap();
        let plan = Arc::new(Plan::build(key, env.column_definitions.as_deref().unwrap_or(&[])));
        let rows = env
            .rows
            .unwrap()
            .iter()
            .filter_map(|r| plan.build_row(&r.cells, r.object))
            .collect();
        (plan, rows)
    }

    #[test]
    fn pod_table_to_rows() {
        let (plan, rows) = parse(POD_TABLE, &kinds::pods());
        let titles: Vec<&str> = plan.columns.iter().map(|c| &*c.title).collect();
        assert_eq!(
            titles,
            ["Name", "Namespace", "Ready", "Status", "Restarts", "IP", "Node", "Nominated Node", "Controlled By", "Age"]
        );
        let visible: Vec<&str> = plan.columns.iter().filter(|c| c.visible).map(|c| &*c.title).collect();
        assert!(visible.contains(&"IP") && visible.contains(&"Node"));
        assert!(!visible.contains(&"Nominated Node"));

        assert_eq!(rows.len(), 3);
        let web = &rows[0];
        assert_eq!(&*web.name, "web-1");
        assert_eq!(&*web.namespace, "shop");
        assert_eq!(web.created, 1704067200);
        assert_eq!(web.tone, Tone::Normal);
        assert_eq!(web.owner_kind(), "ReplicaSet");
        assert_eq!(web.label("app"), Some("web"));
        assert!(web.search.contains("tier=fe"));
        assert!(web.search.contains("node-a"));
        let status = plan.columns.iter().find(|c| &*c.title == "Status").unwrap();
        assert_eq!(web.text(status), "Running");
        assert_eq!(web.cell(status).unwrap().tone, Tone::Ok);

        // Completed pods are not flagged for 0/1 ready.
        assert_eq!(rows[1].tone, Tone::Normal);
        // Crash loops are.
        assert_eq!(rows[2].tone, Tone::Err);
        assert!(rows[2].deleting);
        let restarts = plan.columns.iter().find(|c| &*c.title == "Restarts").unwrap();
        assert_eq!(rows[2].cell(restarts).unwrap().key, 12.0);
    }

    #[test]
    fn watch_events_apply() {
        let (plan, rows) = parse(POD_TABLE, &kinds::pods());
        let store = Arc::new(TableStore::new(kinds::pods(), None));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let _g = rt.enter();
        // A client is required by the struct but never used by `apply`.
        let client = Client::try_from(kube::Config::new("http://127.0.0.1:1".parse().unwrap())).unwrap();
        let mut eng = Engine {
            store,
            client,
            notify: Notifier::noop(),
            rows: rows.into_iter().map(|r| (r.uid.clone(), r)).collect(),
            plan: Some(plan),
            generation: 0,
            state: LoadState::Live,
            error: None,
            error_code: 0,
            handled_refresh: 0,
            listed_once: true,
            poll_only: false,
            watch_retry_at: None,
        };
        let mut rv = "1234".to_string();

        let modified = br#"{"type":"MODIFIED","object":{"kind":"Table","rows":[{"cells":["web-1","0/1","Terminating","0","6m","10.0.0.5","node-a","<none>"],"object":{"metadata":{"name":"web-1","namespace":"shop","uid":"u1","resourceVersion":"1300"}}}]}}"#;
        assert_eq!(eng.apply(modified, &mut rv).unwrap(), (true, false));
        assert_eq!(rv, "1300");
        assert_eq!(eng.rows.len(), 3);
        assert_eq!(eng.rows["u1"].tone, Tone::Warn);

        let deleted = br#"{"type":"DELETED","object":{"kind":"Table","rows":[{"cells":["web-1","0/1","Terminating","0","6m","","",""],"object":{"metadata":{"name":"web-1","namespace":"shop","uid":"u1","resourceVersion":"1301"}}}]}}"#;
        assert_eq!(eng.apply(deleted, &mut rv).unwrap(), (true, false));
        assert_eq!(eng.rows.len(), 2);
        assert_eq!(rv, "1301");

        let bookmark = br#"{"type":"BOOKMARK","object":{"kind":"Table","metadata":{"resourceVersion":"1400"},"rows":[{"cells":["","","","","","","",""],"object":{"metadata":{"resourceVersion":"1400"}}}]}}"#;
        assert_eq!(eng.apply(bookmark, &mut rv).unwrap(), (false, true));
        assert_eq!(rv, "1400");
        assert_eq!(eng.rows.len(), 2);

        let gone = br#"{"type":"ERROR","object":{"kind":"Status","code":410,"message":"too old resource version"}}"#;
        assert!(matches!(eng.apply(gone, &mut rv), Err(Fail::Gone)));
    }

    #[test]
    fn durations_sort_as_time_not_quantities() {
        // "48m" in a Duration column is 48 minutes, not 0.048 of something.
        let text = r#"{"kind":"Table","metadata":{},"columnDefinitions":[
            {"name":"Name","type":"string","priority":0},
            {"name":"Status","type":"string","priority":0},
            {"name":"Duration","type":"string","priority":0},
            {"name":"Created At","type":"date","priority":0},
            {"name":"Age","type":"string","priority":0}],
          "rows":[
            {"cells":["slow","Running","48m","2024-01-01T00:00:00Z","50m"],"object":{"metadata":{"name":"slow","namespace":"b","uid":"1"}}},
            {"cells":["fast","Complete","4s","2024-01-01T00:00:00Z","50m"],"object":{"metadata":{"name":"fast","namespace":"b","uid":"2"}}},
            {"cells":["none","Running","<none>","2024-01-01T00:00:00Z","50m"],"object":{"metadata":{"name":"none","namespace":"b","uid":"3"}}}]}"#;
        let jobs = kinds::builtin("batch", "jobs").unwrap().key();
        let (plan, rows) = parse(text, &jobs);
        let titles: Vec<&str> = plan.columns.iter().map(|c| &*c.title).collect();
        assert_eq!(titles, ["Name", "Namespace", "Status", "Duration", "Controlled By", "Age"], "Created At duplicates Age");
        let duration = plan.columns.iter().find(|c| &*c.title == "Duration").unwrap();
        assert_eq!(duration.kind, ColKind::Time);
        assert_eq!(rows[0].cell(duration).unwrap().key, 48.0 * 60.0);
        assert_eq!(rows[1].cell(duration).unwrap().key, 4.0);
        assert_eq!(crate::table::compare(&rows[1], &rows[0], duration), std::cmp::Ordering::Less);
        assert!(rows[2].is_blank(duration));
        assert!(!rows[0].is_blank(duration));
    }

    /// A minimal API server: answers list requests with a one-row Table and
    /// watch requests with whatever `watch_body` returns, then closes the
    /// connection. Counts requests.
    struct FakeApi {
        addr: std::net::SocketAddr,
        lists: Arc<std::sync::atomic::AtomicUsize>,
        watches: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FakeApi {
        async fn start(watch_body: &'static str) -> FakeApi {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let lists = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let watches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (l, w) = (lists.clone(), watches.clone());
            tokio::spawn(async move {
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else { return };
                    let (l, w) = (l.clone(), w.clone());
                    tokio::spawn(async move {
                        let mut req = Vec::new();
                        let mut chunk = [0u8; 2048];
                        while !req.windows(4).any(|x| x == b"\r\n\r\n") {
                            match sock.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => req.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let head = String::from_utf8_lossy(&req);
                        let body = if head.lines().next().unwrap_or("").contains("watch=1") {
                            w.fetch_add(1, Ordering::SeqCst);
                            watch_body.to_string()
                        } else {
                            l.fetch_add(1, Ordering::SeqCst);
                            r#"{"kind":"Table","metadata":{"resourceVersion":"5"},"columnDefinitions":[{"name":"Name","type":"string","priority":0}],"rows":[{"cells":["a"],"object":{"metadata":{"name":"a","namespace":"x","uid":"u-a","resourceVersion":"5"}}}]}"#.to_string()
                        };
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                        let _ = sock.shutdown().await;
                    });
                }
            });
            FakeApi { addr, lists, watches }
        }

        fn store(&self) -> Arc<TableStore> {
            let store = Arc::new(TableStore::new(kinds::builtin("", "configmaps").unwrap().key(), None));
            let client = Client::try_from(kube::Config::new(format!("http://{}", self.addr).parse().unwrap())).unwrap();
            tokio::spawn(run(store.clone(), client, Notifier::noop()));
            store
        }

        fn counts(&self) -> (usize, usize) {
            (self.lists.load(Ordering::SeqCst), self.watches.load(Ordering::SeqCst))
        }
    }

    async fn until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The API server reports an expired resourceVersion as an in-stream
    /// ERROR event on a 200 response. If that keeps happening the engine
    /// must back off instead of listing and watching in a tight loop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn expired_watches_back_off() {
        let api = FakeApi::start(
            "{\"type\":\"ERROR\",\"object\":{\"kind\":\"Status\",\"code\":410,\"reason\":\"Expired\",\"message\":\"too old resource version\"}}\n",
        )
        .await;
        let store = api.store();
        until("first rows", Duration::from_secs(5), || !store.peek().rows.is_empty()).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let (lists, watches) = api.counts();
        // Backoff of 0.5 s, 1 s, 2 s ... allows only a handful in 3 s.
        assert!(lists <= 6 && watches <= 6, "tight loop: {lists} lists and {watches} watches in 3 s");
        assert!(lists >= 2, "it must keep retrying: {lists} lists");
        assert_eq!(store.peek().rows.len(), 1, "rows stay available meanwhile");
    }

    /// Something between us and the server that ends every watch at once:
    /// stop claiming to be live and poll instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watches_that_end_immediately_fall_back_to_polling() {
        let api = FakeApi::start("").await;
        let store = api.store();
        until("polling", Duration::from_secs(8), || store.peek().state == LoadState::Polling).await;
        let (_, watches) = api.counts();
        assert!(watches <= 4, "gave up watching after a few attempts, not {watches}");
        assert_eq!(store.peek().rows.len(), 1);
    }

    /// A refresh requested while the engine is pausing between watch
    /// attempts must cause a new list, not just another watch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_during_backoff_relists() {
        let api = FakeApi::start("").await;
        let store = api.store();
        // Wait until the first watch has ended and the engine is pausing.
        until("first watch", Duration::from_secs(5), || api.counts().1 >= 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let lists_before = api.counts().0;
        store.refresh();
        until("relist after refresh", Duration::from_secs(2), || api.counts().0 > lists_before).await;
    }

    #[test]
    fn plain_list_fallback() {
        let text = r#"{"kind":"PodMetricsList","metadata":{},"items":[{"metadata":{"name":"a","namespace":"x","creationTimestamp":"2024-01-01T00:00:00Z"}}]}"#;
        let env: Envelope = serde_json::from_str(text).unwrap();
        assert_ne!(env.kind, "Table");
        let plan = Plan::build(&kinds::pods(), &[]);
        let row = plan.build_row(&[], Some(env.items.unwrap()[0])).unwrap();
        assert_eq!(&*row.name, "a");
        assert_eq!(&*row.uid, "x/a");
    }
}
