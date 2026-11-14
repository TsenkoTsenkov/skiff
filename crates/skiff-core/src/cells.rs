//! Turning raw values into display cells: sort keys, status tones, ages.

use serde_json::Value;

/// Semantic colouring of a cell or row. Ordered so `max` yields the worst.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tone {
    #[default]
    Normal,
    Muted,
    Ok,
    Info,
    Warn,
    Err,
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub text: Box<str>,
    /// Numeric sort key; NaN means "compare by text".
    pub key: f64,
    pub tone: Tone,
}

impl Cell {
    pub fn text(s: impl Into<Box<str>>) -> Self {
        Cell { text: s.into(), key: f64::NAN, tone: Tone::Normal }
    }
    pub fn empty() -> Self {
        Cell { text: "".into(), key: f64::NAN, tone: Tone::Muted }
    }
    pub fn num(n: f64, text: impl Into<Box<str>>) -> Self {
        Cell { text: text.into(), key: n, tone: Tone::Normal }
    }
    /// A point in time (unix seconds); rendered as a relative age by the UI.
    pub fn time(unix: i64) -> Self {
        if unix <= 0 {
            return Cell::empty();
        }
        Cell { text: "".into(), key: unix as f64, tone: Tone::Normal }
    }
    pub fn toned(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

/// RFC 3339 timestamp to unix seconds; 0 when absent or malformed.
pub fn parse_time(s: &str) -> i64 {
    s.parse::<jiff::Timestamp>().map(|t| t.as_second()).unwrap_or(0)
}

pub fn now_unix() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// kubectl-style compact duration ("45s", "5m10s", "3h", "2d4h", "1y20d").
pub fn fmt_age(secs: i64) -> String {
    if secs < 0 {
        return "0s".into();
    }
    let s = secs;
    let m = s / 60;
    let h = m / 60;
    let d = h / 24;
    let y = d / 365;
    if s < 120 {
        format!("{s}s")
    } else if m < 10 {
        let rem = s % 60;
        if rem == 0 { format!("{m}m") } else { format!("{m}m{rem}s") }
    } else if h < 3 {
        format!("{m}m")
    } else if h < 8 {
        let rem = m % 60;
        if rem == 0 { format!("{h}h") } else { format!("{h}h{rem}m") }
    } else if h < 48 {
        format!("{h}h")
    } else if d < 8 {
        let rem = h % 24;
        if rem == 0 { format!("{d}d") } else { format!("{d}d{rem}h") }
    } else if y < 2 {
        format!("{d}d")
    } else if y < 8 {
        let rem = d % 365;
        if rem == 0 { format!("{y}y") } else { format!("{y}y{rem}d") }
    } else {
        format!("{y}y")
    }
}

/// Parses "3d4h", "5m10s", "45s", "2y" into seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0f64;
    let mut num = String::new();
    let mut seen_unit = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else {
            let n: f64 = num.parse().ok()?;
            num.clear();
            let mult = match ch {
                's' => 1.0,
                'm' => 60.0,
                'h' => 3600.0,
                'd' => 86400.0,
                'y' => 86400.0 * 365.0,
                _ => return None,
            };
            total += n * mult;
            seen_unit = true;
        }
    }
    if !num.is_empty() || !seen_unit {
        return None;
    }
    Some(total)
}

/// Parses a Kubernetes quantity ("250m", "1.5Gi", "2", "128974848") to a
/// plain number in base units.
pub fn parse_quantity(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(s.len());
    let (num, suffix) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult = match suffix {
        "" => 1.0,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024.0,
        "Mi" => 1024.0 * 1024.0,
        "Gi" => 1024.0 * 1024.0 * 1024.0,
        "Ti" => 1024.0f64.powi(4),
        "Pi" => 1024.0f64.powi(5),
        "Ei" => 1024.0f64.powi(6),
        _ => return None,
    };
    Some(n * mult)
}

pub fn fmt_bytes(b: f64) -> String {
    const GI: f64 = 1024.0 * 1024.0 * 1024.0;
    const MI: f64 = 1024.0 * 1024.0;
    const KI: f64 = 1024.0;
    if b >= 10.0 * GI {
        format!("{:.0}Gi", b / GI)
    } else if b >= GI {
        format!("{:.1}Gi", b / GI)
    } else if b >= MI {
        format!("{:.0}Mi", b / MI)
    } else if b >= KI {
        format!("{:.0}Ki", b / KI)
    } else {
        format!("{b:.0}")
    }
}

/// CPU in cores to "250m" / "1.50".
pub fn fmt_cpu(cores: f64) -> String {
    if cores < 1.0 {
        format!("{:.0}m", cores * 1000.0)
    } else if cores < 10.0 {
        format!("{cores:.2}")
    } else {
        format!("{cores:.1}")
    }
}

/// "2/3" style readiness fractions.
pub fn parse_fraction(s: &str) -> Option<(f64, f64)> {
    let (a, b) = s.split_once('/')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn word_tone(w: &str) -> Tone {
    let w = w.trim();
    if w.is_empty() {
        return Tone::Normal;
    }
    if let Some(rest) = w.strip_prefix("Init:") {
        // "Init:1/3" is progress, "Init:CrashLoopBackOff" is a failure.
        return if rest.starts_with(|c: char| c.is_ascii_digit()) {
            Tone::Warn
        } else {
            word_tone(rest).max(Tone::Warn)
        };
    }
    let mut buf = [0u8; 40];
    let bytes = w.as_bytes();
    if bytes.len() > buf.len() {
        return Tone::Normal;
    }
    for (i, b) in bytes.iter().enumerate() {
        buf[i] = b.to_ascii_lowercase();
    }
    let lower = std::str::from_utf8(&buf[..bytes.len()]).unwrap_or("");
    match lower {
        "running" | "active" | "bound" | "ready" | "healthy" | "synced" | "available" | "deployed"
        | "established" | "true" | "ok" | "valid" | "accepted" | "programmed" | "up" => Tone::Ok,
        "completed" | "succeeded" | "complete" | "normal" | "terminated" => Tone::Muted,
        "pending" | "containercreating" | "podinitializing" | "terminating" | "progressing" | "released"
        | "unknown" | "outofsync" | "schedulingdisabled" | "waiting" | "warning" | "suspended"
        | "paused" | "superseded" | "uninstalling" | "pending-install" | "pending-upgrade"
        | "pending-rollback" | "updating" | "provisioning" | "deleting" | "scaling" | "reconciling" => Tone::Warn,
        "failed" | "error" | "crashloopbackoff" | "imagepullbackoff" | "errimagepull" | "oomkilled"
        | "evicted" | "lost" | "false" | "degraded" | "missing" | "invalidimagename"
        | "createcontainerconfigerror" | "createcontainererror" | "runcontainererror"
        | "deadlineexceeded" | "backofflimitexceeded" | "unschedulable" | "nodelost"
        | "containercannotrun" | "starterror" | "unhealthy" | "errimageneverpull" | "notready"
        | "backoff" | "failedscheduling" | "failedmount" | "killing" | "nodenotready" | "down" => Tone::Err,
        _ => Tone::Normal,
    }
}

/// Tone for a status-like value; comma separated values take the worst.
pub fn status_tone(s: &str) -> Tone {
    s.split(',').map(word_tone).max().unwrap_or(Tone::Normal)
}

/// Whether a server column should be interpreted as a status.
pub fn is_status_column(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "status" | "phase" | "state" | "health" | "ready" | "available" | "synced" | "healthy" | "type"
            | "reason" | "sync status" | "health status" | "established" | "condition"
    )
}

pub fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a.iter().map(value_text).collect::<Vec<_>>().join(", "),
        Value::Object(_) => v.to_string(),
    }
}

/// How a server column's values should be interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColKind {
    Text,
    Number,
    /// Unix timestamp rendered as relative age.
    Time,
    Status,
}

/// Builds a cell from a server-rendered Table value.
pub fn cell_from_value(v: &Value, kind: ColKind) -> Cell {
    if let Value::Number(n) = v {
        let f = n.as_f64().unwrap_or(f64::NAN);
        return Cell { text: n.to_string().into(), key: f, tone: Tone::Normal };
    }
    let text = value_text(v);
    if text.is_empty() || text == "<none>" || text == "<unset>" || text == "<unknown>" || text == "<invalid>" {
        return Cell::empty();
    }
    match kind {
        ColKind::Time => {
            let t = parse_time(&text);
            if t > 0 {
                return Cell::time(t);
            }
            // Server already rendered a relative duration.
            let key = parse_duration(&text).unwrap_or(f64::NAN);
            Cell { text: text.into(), key, tone: Tone::Normal }
        }
        ColKind::Status => {
            if let Some((a, b)) = parse_fraction(&text) {
                let tone = if a < b { Tone::Warn } else { Tone::Normal };
                let key = if b > 0.0 { a / b } else { 1.0 };
                return Cell { text: text.into(), key, tone };
            }
            let tone = status_tone(&text);
            Cell { text: text.into(), key: f64::NAN, tone }
        }
        ColKind::Number | ColKind::Text => {
            let key = sort_key(&text);
            Cell { text: text.into(), key, tone: Tone::Normal }
        }
    }
}

/// Best-effort numeric interpretation of free text for sorting.
pub fn sort_key(text: &str) -> f64 {
    let t = text.trim();
    let first = match t.as_bytes().first() {
        Some(b) => *b,
        None => return f64::NAN,
    };
    if !(first.is_ascii_digit() || first == b'-' || first == b'.') {
        return f64::NAN;
    }
    if let Ok(n) = t.parse::<f64>() {
        return n;
    }
    if let Some((a, b)) = parse_fraction(t) {
        return if b > 0.0 { a / b } else { 1.0 };
    }
    if let Some(q) = parse_quantity(t) {
        return q;
    }
    if let Some(d) = parse_duration(t) {
        return d;
    }
    // "3 (5m ago)" restart counts and similar: leading integer.
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && t[digits.len()..].starts_with(' ') {
        return digits.parse().unwrap_or(f64::NAN);
    }
    f64::NAN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(fmt_age(5), "5s");
        assert_eq!(fmt_age(119), "119s");
        assert_eq!(fmt_age(125), "2m5s");
        assert_eq!(fmt_age(3600), "60m");
        assert_eq!(fmt_age(4 * 3600 + 120), "4h2m");
        assert_eq!(fmt_age(30 * 3600), "30h");
        assert_eq!(fmt_age(3 * 86400 + 7200), "3d2h");
        assert_eq!(fmt_age(40 * 86400), "40d");
        assert_eq!(fmt_age(3 * 365 * 86400 + 5 * 86400), "3y5d");
    }

    #[test]
    fn durations_and_quantities() {
        assert_eq!(parse_duration("3d4h"), Some(3.0 * 86400.0 + 4.0 * 3600.0));
        assert_eq!(parse_duration("45s"), Some(45.0));
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("12"), None);
        assert_eq!(parse_quantity("250m"), Some(0.25));
        assert_eq!(parse_quantity("2Gi"), Some(2.0 * 1024.0 * 1024.0 * 1024.0));
        assert_eq!(parse_quantity("8"), Some(8.0));
        assert_eq!(parse_quantity("8xyz"), None);
        assert_eq!(fmt_bytes(1536.0 * 1024.0 * 1024.0), "1.5Gi");
        assert_eq!(fmt_cpu(0.25), "250m");
    }

    #[test]
    fn tones() {
        assert_eq!(status_tone("Running"), Tone::Ok);
        assert_eq!(status_tone("CrashLoopBackOff"), Tone::Err);
        assert_eq!(status_tone("Ready,SchedulingDisabled"), Tone::Warn);
        assert_eq!(status_tone("NotReady"), Tone::Err);
        assert_eq!(status_tone("Init:0/2"), Tone::Warn);
        assert_eq!(status_tone("Init:ErrImagePull"), Tone::Err);
        assert_eq!(status_tone("Completed"), Tone::Muted);
        assert_eq!(status_tone("ClusterIP"), Tone::Normal);
    }

    #[test]
    fn sort_keys() {
        assert_eq!(sort_key("12"), 12.0);
        assert_eq!(sort_key("3 (5m ago)"), 3.0);
        assert_eq!(sort_key("1/2"), 0.5);
        assert_eq!(sort_key("5m10s"), 310.0);
        assert!(sort_key("nginx").is_nan());
        assert!(sort_key("10.0.0.1").is_nan() || sort_key("10.0.0.1") == 10.0);
    }

    #[test]
    fn cells() {
        let c = cell_from_value(&Value::String("1/3".into()), ColKind::Status);
        assert_eq!(c.tone, Tone::Warn);
        let c = cell_from_value(&Value::String("<none>".into()), ColKind::Text);
        assert_eq!(c.tone, Tone::Muted);
        assert!(c.text.is_empty());
        let c = cell_from_value(&Value::String("2024-01-01T00:00:00Z".into()), ColKind::Time);
        assert_eq!(c.key, 1704067200.0);
    }
}
