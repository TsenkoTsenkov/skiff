//! The tabular model the UI renders: columns, compact rows and immutable
//! snapshots published by the watch engine.

use std::cmp::Ordering;
use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

pub use crate::cells::{Cell, ColKind, Tone};
use crate::cells::now_unix;
use crate::kinds::ResourceKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColSource {
    Name,
    Namespace,
    Age,
    Owner,
    /// Index into `Row::cells`.
    Cell(usize),
}

#[derive(Clone, Debug)]
pub struct Column {
    pub title: Box<str>,
    pub source: ColSource,
    pub kind: ColKind,
    /// Shown unless the user toggles it; "wide" columns start hidden.
    pub visible: bool,
}

/// One object, reduced to what a table needs. Deliberately small: the full
/// object is fetched on demand when details are opened.
#[derive(Debug)]
pub struct Row {
    pub uid: Box<str>,
    pub name: Box<str>,
    /// Empty for cluster-scoped objects.
    pub namespace: Box<str>,
    /// Creation time, unix seconds (0 if unknown).
    pub created: i64,
    pub deleting: bool,
    /// Worst status tone on the row.
    pub tone: Tone,
    pub rv: Box<str>,
    pub cells: Box<[Cell]>,
    /// Controlling owner as (kind, name).
    pub owner: Option<Box<(Box<str>, Box<str>)>>,
    /// `k=v` pairs separated by spaces.
    pub labels: Box<str>,
    /// Lower-cased text used by the quick filter.
    pub search: Box<str>,
}

impl Row {
    pub fn text<'a>(&'a self, col: &Column) -> &'a str {
        match col.source {
            ColSource::Name => &self.name,
            ColSource::Namespace => &self.namespace,
            ColSource::Age => "",
            ColSource::Owner => self.owner.as_ref().map(|o| &*o.1).unwrap_or(""),
            ColSource::Cell(i) => self.cells.get(i).map(|c| &*c.text).unwrap_or(""),
        }
    }

    pub fn cell(&self, col: &Column) -> Option<&Cell> {
        match col.source {
            ColSource::Cell(i) => self.cells.get(i),
            _ => None,
        }
    }

    pub fn owner_kind(&self) -> &str {
        self.owner.as_ref().map(|o| &*o.0).unwrap_or("")
    }

    /// Value of a label, if present.
    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels.split(' ').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then_some(v)
        })
    }
}

/// Orders "web-2" before "web-10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (mut da, mut db) = (&a[si..i], &b[sj..j]);
            while da.len() > 1 && da[0] == b'0' {
                da = &da[1..];
            }
            while db.len() > 1 && db[0] == b'0' {
                db = &db[1..];
            }
            let ord = da.len().cmp(&db.len()).then_with(|| da.cmp(db));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            let (ca, cb) = (a[i].to_ascii_lowercase(), b[j].to_ascii_lowercase());
            if ca != cb {
                return ca.cmp(&cb);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Ascending comparison of two rows by a column. Time-like columns order by
/// age, so ascending means most recent first.
pub fn compare(a: &Row, b: &Row, col: &Column) -> Ordering {
    let ord = match col.source {
        ColSource::Name => natural_cmp(&a.name, &b.name),
        ColSource::Namespace => a.namespace.cmp(&b.namespace),
        ColSource::Age => b.created.cmp(&a.created),
        ColSource::Owner => natural_cmp(a.text(col), b.text(col)),
        ColSource::Cell(i) => match (a.cells.get(i), b.cells.get(i)) {
            (Some(ca), Some(cb)) => match (ca.key.is_nan(), cb.key.is_nan()) {
                (false, false) => {
                    let o = ca.key.partial_cmp(&cb.key).unwrap_or(Ordering::Equal);
                    if col.kind == ColKind::Time && ca.text.is_empty() && cb.text.is_empty() {
                        o.reverse()
                    } else {
                        o
                    }
                }
                (true, true) => match (ca.text.is_empty(), cb.text.is_empty()) {
                    // Empty values sink to the bottom in ascending order.
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => natural_cmp(&ca.text, &cb.text),
                },
                (false, true) => Ordering::Less,
                (true, false) => Ordering::Greater,
            },
            _ => Ordering::Equal,
        },
    };
    ord.then_with(|| a.namespace.cmp(&b.namespace))
        .then_with(|| natural_cmp(&a.name, &b.name))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// Waiting for the cluster connection.
    Connecting,
    /// Initial list in progress; rows may already be partially present.
    Loading,
    /// Fully listed and receiving watch events.
    Live,
    /// Server does not support watch for this view; re-listed periodically.
    Polling,
    /// Connection lost; rows are stale while we retry.
    Reconnecting,
    Failed,
}

#[derive(Debug)]
pub struct Snapshot {
    /// Increments on every publish; lets the UI cache derived data.
    pub generation: u64,
    pub columns: Arc<[Column]>,
    pub rows: Vec<Arc<Row>>,
    pub state: LoadState,
    pub error: Option<Box<str>>,
    /// HTTP status of `error` when it came from the API server.
    pub error_code: u16,
    /// (column index, descending).
    pub default_sort: Option<(usize, bool)>,
}

impl Snapshot {
    pub fn empty(state: LoadState) -> Self {
        Snapshot {
            generation: 0,
            columns: Arc::from(Vec::new()),
            rows: Vec::new(),
            state,
            error: None,
            error_code: 0,
            default_sort: None,
        }
    }

    pub fn column(&self, title: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.title.eq_ignore_ascii_case(title))
    }

    pub fn find(&self, namespace: &str, name: &str) -> Option<&Arc<Row>> {
        self.rows.iter().find(|r| &*r.name == name && &*r.namespace == namespace)
    }
}

/// A live table for one resource type (optionally scoped to a namespace).
pub struct TableStore {
    pub key: ResourceKey,
    /// Namespace scope of the watch; `None` watches cluster-wide.
    pub scope: Option<Box<str>>,
    snap: ArcSwap<Snapshot>,
    last_used: AtomicI64,
    pub(crate) relist: Notify,
    pub(crate) abort: Mutex<Option<AbortHandle>>,
}

impl TableStore {
    pub(crate) fn new(key: ResourceKey, scope: Option<Box<str>>) -> Self {
        TableStore {
            key,
            scope,
            snap: ArcSwap::from_pointee(Snapshot::empty(LoadState::Connecting)),
            last_used: AtomicI64::new(now_unix()),
            relist: Notify::new(),
            abort: Mutex::new(None),
        }
    }

    /// Current contents. Cheap (one atomic load); call every frame.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.last_used.store(now_unix(), AtomicOrdering::Relaxed);
        self.snap.load_full()
    }

    /// Like `snapshot` but does not count as use for idle eviction.
    pub fn peek(&self) -> Arc<Snapshot> {
        self.snap.load_full()
    }

    pub(crate) fn publish(&self, snap: Snapshot) {
        self.snap.store(Arc::new(snap));
    }

    pub(crate) fn idle_secs(&self) -> i64 {
        now_unix() - self.last_used.load(AtomicOrdering::Relaxed)
    }

    /// Drop cached rows and list again from scratch.
    pub fn refresh(&self) {
        self.relist.notify_one();
    }

    pub(crate) fn stop(&self) {
        if let Some(h) = self.abort.lock().take() {
            h.abort();
        }
    }
}

impl Drop for TableStore {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("web-2", "web-10"), Ordering::Less);
        assert_eq!(natural_cmp("web-10", "web-2"), Ordering::Greater);
        assert_eq!(natural_cmp("a", "b"), Ordering::Less);
        assert_eq!(natural_cmp("node-01", "node-1"), Ordering::Equal);
        assert_eq!(natural_cmp("abc", "abcd"), Ordering::Less);
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
    }
}
