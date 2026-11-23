//! Where kubeconfigs come from, and loading them the way kubectl does.
//!
//! kube's own `Kubeconfig::read()` fails as a whole when one file listed in
//! `$KUBECONFIG` is missing or invalid, and only knows the environment. A
//! desktop app needs more: files added from the UI, directories of
//! per-cluster files, and a result that still lists every usable context
//! while saying exactly which file is broken.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use kube::config::{AuthInfo, Cluster, ExecConfig, Kubeconfig, KubeconfigError};

use crate::session::{ContextInfo, Contexts};

/// Where kubeconfigs are read from.
///
/// Either the kubectl default (the `$KUBECONFIG` list, else
/// `~/.kube/config`) or an explicit list of entries. An entry is a file, a
/// directory (every file directly in it), and may start with `~` and use
/// `*` / `?` wildcards in its last path component
/// (`~/.kube/conf.d/*.yaml`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KubeconfigSources {
    /// `None`: the kubectl default, re-evaluated on every load.
    entries: Option<Vec<String>>,
}

impl KubeconfigSources {
    /// `$KUBECONFIG` (a path list), else `~/.kube/config`.
    pub fn from_env() -> Self {
        KubeconfigSources { entries: None }
    }

    /// An explicit list; nothing else is read. Empty entries are ignored.
    pub fn explicit<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let entries = entries.into_iter().map(Into::into).filter(|e: &String| !e.trim().is_empty()).collect();
        KubeconfigSources { entries: Some(entries) }
    }

    /// True when this is the kubectl default rather than an explicit list.
    pub fn is_default(&self) -> bool {
        self.entries.is_none()
    }

    /// The explicit entries as given (unexpanded); `None` for the default.
    pub fn entries(&self) -> Option<&[String]> {
        self.entries.as_deref()
    }

    /// The same sources with one more entry. Starting from the default
    /// keeps what the default currently resolves to, so adding a file from
    /// the UI does not hide the clusters that were already listed.
    pub fn with_entry(&self, entry: impl Into<String>) -> Self {
        let mut entries: Vec<String> = match &self.entries {
            Some(e) => e.clone(),
            None => default_paths().iter().map(|p| p.to_string_lossy().into_owned()).collect(),
        };
        let entry = entry.into();
        if !entry.trim().is_empty() && !entries.contains(&entry) {
            entries.push(entry);
        }
        KubeconfigSources { entries: Some(entries) }
    }

    /// The candidate files, in merge order, without duplicates. Files that
    /// do not exist are still listed (for "looked in ..." messages) unless
    /// they came from a directory or a wildcard.
    pub fn resolve(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        match &self.entries {
            None => out = default_paths(),
            Some(entries) => {
                for e in entries {
                    expand_entry(e, &mut out);
                }
            }
        }
        let mut seen = HashSet::new();
        out.retain(|p| seen.insert(p.clone()));
        out
    }
}

/// A kubeconfig file that exists but could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeconfigProblem {
    pub path: PathBuf,
    /// Short reason, without the path.
    pub message: String,
}

impl std::fmt::Display for KubeconfigProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

/// Result of loading kubeconfigs. Never an error as a whole: broken files
/// are listed in `problems` and the rest is still merged.
#[derive(Clone, Debug, Default)]
pub struct LoadedKubeconfig {
    pub contexts: Contexts,
    /// Files that were read and merged, in order.
    pub files: Vec<PathBuf>,
    /// Every file that was looked for, including missing ones.
    pub searched: Vec<PathBuf>,
    /// Files that exist but are unreadable or invalid.
    pub problems: Vec<KubeconfigProblem>,
}

/// Reads and merges the kubeconfigs of `sources` and lists their contexts.
pub fn load(sources: &KubeconfigSources) -> LoadedKubeconfig {
    let merged = read_merged(sources);
    let contexts = contexts_of(&merged.config, &merged.origin);
    LoadedKubeconfig { contexts, files: merged.files, searched: merged.searched, problems: merged.problems }
}

/// The merged kubeconfig plus bookkeeping; used for listing and for
/// building clients, so both always see the same files.
pub(crate) struct Merged {
    pub config: Kubeconfig,
    pub files: Vec<PathBuf>,
    pub searched: Vec<PathBuf>,
    pub problems: Vec<KubeconfigProblem>,
    /// Context name -> the file that defined it.
    pub origin: HashMap<String, PathBuf>,
}

pub(crate) fn read_merged(sources: &KubeconfigSources) -> Merged {
    let searched = sources.resolve();
    let mut merged = Merged {
        config: Kubeconfig::default(),
        files: Vec::new(),
        searched: searched.clone(),
        problems: Vec::new(),
        origin: HashMap::new(),
    };
    for path in searched {
        // kubectl skips files that are not there; anything else that goes
        // wrong with a file that exists is worth telling the user about.
        match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                merged.problems.push(KubeconfigProblem { path, message: io_message(&e) });
                continue;
            }
            Ok(m) if m.is_dir() => continue,
            Ok(_) => {}
        }
        // `read_from` also resolves relative certificate, key, token-file
        // and exec paths against the file's directory.
        let kc = match Kubeconfig::read_from(&path) {
            Ok(kc) => kc,
            Err(e) => {
                merged.problems.push(KubeconfigProblem { path, message: problem_message(&e) });
                continue;
            }
        };
        let names: Vec<String> = kc.contexts.iter().map(|c| c.name.clone()).collect();
        // First definition of a cluster, user or context name wins, and
        // current-context comes from the first file that sets one.
        match merged.config.clone().merge(kc) {
            Ok(m) => merged.config = m,
            Err(e) => {
                merged.problems.push(KubeconfigProblem { path, message: problem_message(&e) });
                continue;
            }
        }
        for name in names {
            merged.origin.entry(name).or_insert_with(|| path.clone());
        }
        merged.files.push(path);
    }
    merged
}

fn io_message(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        std::io::ErrorKind::InvalidData => "not a text file".to_string(),
        _ => e.to_string(),
    }
}

fn problem_message(e: &KubeconfigError) -> String {
    match e {
        KubeconfigError::ReadConfig(io, _) => io_message(io),
        KubeconfigError::Parse(inner) => {
            // Parser messages can run over several lines with a snippet of
            // the file; the first line says what and where.
            let text = inner.to_string();
            let first = text.lines().next().unwrap_or_default();
            let short: String = first.chars().take(200).collect();
            format!("not a valid kubeconfig ({short})")
        }
        other => other.to_string(),
    }
}

pub(crate) fn cluster_of<'a>(kc: &'a Kubeconfig, name: &str) -> Option<&'a Cluster> {
    kc.clusters.iter().find(|n| n.name == name).and_then(|n| n.cluster.as_ref())
}

pub(crate) fn auth_of<'a>(kc: &'a Kubeconfig, user: &str) -> Option<&'a AuthInfo> {
    kc.auth_infos.iter().find(|n| n.name == user).and_then(|n| n.auth_info.as_ref())
}

/// The exec credential plugin a context's user is configured with.
pub(crate) fn exec_of<'a>(kc: &'a Kubeconfig, context: &str) -> Option<&'a ExecConfig> {
    let ctx = kc.contexts.iter().find(|c| c.name == context)?.context.as_ref()?;
    auth_of(kc, ctx.user.as_deref()?)?.exec.as_ref()
}

/// The name to show for a plugin command: `aws`, not `/usr/local/bin/aws`.
pub(crate) fn command_name(command: &str) -> String {
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command);
    if base.is_empty() {
        command.to_string()
    } else {
        base.to_string()
    }
}

fn contexts_of(kc: &Kubeconfig, origin: &HashMap<String, PathBuf>) -> Contexts {
    let items = kc
        .contexts
        .iter()
        .map(|c| {
            let ctx = c.context.as_ref();
            let cluster_name = ctx.map(|c| c.cluster.clone()).unwrap_or_default();
            let user = ctx.and_then(|c| c.user.clone()).unwrap_or_default();
            let cluster = cluster_of(kc, &cluster_name);
            let server = cluster.and_then(|c| c.server.clone()).unwrap_or_default();
            let insecure = cluster.and_then(|c| c.insecure_skip_tls_verify).unwrap_or(false)
                || server.trim_start().to_ascii_lowercase().starts_with("http://");
            let exec_command =
                auth_of(kc, &user).and_then(|a| a.exec.as_ref()).and_then(|e| e.command.as_deref()).map(command_name);
            ContextInfo {
                name: c.name.clone(),
                namespace: ctx.and_then(|c| c.namespace.clone()),
                proxy: cluster.and_then(|c| c.proxy_url.clone()).filter(|p| !p.is_empty()),
                file: origin.get(&c.name).cloned(),
                cluster: cluster_name,
                user,
                server,
                insecure,
                exec_command,
            }
        })
        .collect();
    Contexts { current: kc.current_context.clone(), items }
}

/// The user's home directory, from the environment.
pub(crate) fn home_dir() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        var("USERPROFILE").or_else(|| var("HOME"))
    } else {
        var("HOME")
    }
}

fn default_paths() -> Vec<PathBuf> {
    paths_from_env(std::env::var_os("KUBECONFIG"), home_dir())
}

fn paths_from_env(kubeconfig: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let listed: Vec<PathBuf> = kubeconfig
        .map(|v| std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect())
        .unwrap_or_default();
    if !listed.is_empty() {
        return listed;
    }
    home.map(|h| vec![h.join(".kube").join("config")]).unwrap_or_default()
}

fn expand_tilde(entry: &str, home: Option<&Path>) -> PathBuf {
    let rest = match entry.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => rest,
        _ => return PathBuf::from(entry),
    };
    match home {
        Some(home) => {
            let rest = rest.trim_start_matches(['/', '\\']);
            if rest.is_empty() {
                home.to_path_buf()
            } else {
                home.join(rest)
            }
        }
        None => PathBuf::from(entry),
    }
}

fn expand_entry(entry: &str, out: &mut Vec<PathBuf>) {
    expand_entry_with(entry.trim(), home_dir().as_deref(), out)
}

fn expand_entry_with(entry: &str, home: Option<&Path>, out: &mut Vec<PathBuf>) {
    if entry.is_empty() {
        return;
    }
    let path = expand_tilde(entry, home);
    let pattern = path.file_name().and_then(|n| n.to_str()).filter(|n| n.contains(['*', '?']));
    if let Some(pattern) = pattern {
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        out.extend(files_in(&dir, |name| wildcard_match(pattern, name)));
    } else if path.is_dir() {
        // A directory of kubeconfigs. Editor backups and dot files are not
        // kubeconfigs, and would only produce "invalid file" noise.
        out.extend(files_in(&path, |name| !name.starts_with('.') && !name.ends_with('~')));
    } else {
        out.push(path);
    }
}

/// Regular files directly in `dir` whose name passes `keep`, sorted by name
/// so the merge order is stable.
fn files_in(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_str().is_some_and(&keep))
        .map(|e| e.path())
        // Follows symlinks: a link to a file counts, a directory does not.
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found
}

/// `*` matches any run of characters, `?` exactly one.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    // Where to resume when a later literal fails: (pattern index after the
    // last `*`, name index that `*` should swallow next).
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi + 1, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Finds an executable the way the OS would when spawning `command`:
/// a name is searched on `PATH`, anything with a path separator is checked
/// as given.
pub fn find_executable(command: &str) -> Option<PathBuf> {
    find_executable_in(command, std::env::var_os("PATH"))
}

fn find_executable_in(command: &str, path: Option<OsString>) -> Option<PathBuf> {
    if command.is_empty() {
        return None;
    }
    let has_separator = command.contains('/') || (cfg!(windows) && command.contains('\\'));
    if has_separator || Path::new(command).is_absolute() {
        return executable_candidate(Path::new(command));
    }
    std::env::split_paths(&path?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| executable_candidate(&dir.join(command)))
}

#[cfg(unix)]
fn executable_candidate(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then(|| path.to_path_buf())
}

#[cfg(not(unix))]
fn executable_candidate(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    // `aws` on Windows is `aws.exe` (or `.cmd`, ...): try the extensions
    // the shell would.
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    exts.split(';').filter(|e| !e.is_empty()).find_map(|ext| {
        let mut name = path.as_os_str().to_os_string();
        name.push(ext);
        let candidate = PathBuf::from(name);
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "skiff-kc-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            TempDir(dir)
        }
        fn write(&self, name: &str, text: &str) -> PathBuf {
            let p = self.0.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            std::fs::write(&p, text).expect("write");
            p
        }
        fn entry(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config(context: &str, cluster: &str, server: &str, extra_cluster: &str, user_body: &str) -> String {
        format!(
            "apiVersion: v1\nkind: Config\ncurrent-context: {context}\nclusters:\n- name: {cluster}\n  cluster:\n    server: {server}\n{extra_cluster}contexts:\n- name: {context}\n  context:\n    cluster: {cluster}\n    user: {context}-user\n    namespace: team\nusers:\n- name: {context}-user\n  user:\n{user_body}"
        )
    }

    #[test]
    fn missing_files_are_skipped_and_broken_ones_reported() {
        let t = TempDir::new("merge");
        let a = t.write("a.yaml", &config("alpha", "c1", "https://one.example:6443", "", "    token: x\n"));
        let bad = t.write("bad.yaml", "clusters: [this is: not: valid\n");
        let b = t.write("b.yaml", &config("beta", "c2", "https://two.example", "", "    token: y\n"));
        let sources = KubeconfigSources::explicit([
            t.entry("a.yaml"),
            t.entry("gone.yaml"),
            t.entry("bad.yaml"),
            t.entry("b.yaml"),
        ]);
        let loaded = load(&sources);

        let names: Vec<&str> = loaded.contexts.items.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"]);
        assert_eq!(loaded.contexts.current.as_deref(), Some("alpha"), "first file to set current-context wins");
        assert_eq!(loaded.files, [a.clone(), b.clone()]);
        assert_eq!(loaded.searched.len(), 4);
        assert_eq!(loaded.problems.len(), 1);
        assert_eq!(loaded.problems[0].path, bad);
        assert!(loaded.problems[0].message.contains("not a valid kubeconfig"), "{}", loaded.problems[0].message);
        assert_eq!(loaded.contexts.items[0].file.as_deref(), Some(a.as_path()));
        assert_eq!(loaded.contexts.items[1].file.as_deref(), Some(b.as_path()));
        assert_eq!(loaded.contexts.items[0].namespace.as_deref(), Some("team"));
    }

    #[test]
    fn first_definition_of_a_name_wins() {
        let t = TempDir::new("first");
        let a = t.write("a.yaml", &config("same", "c", "https://first.example", "", "    token: x\n"));
        t.write("b.yaml", &config("same", "c", "https://second.example", "", "    token: y\n"));
        let loaded = load(&KubeconfigSources::explicit([t.entry("a.yaml"), t.entry("b.yaml")]));
        assert_eq!(loaded.contexts.items.len(), 1);
        assert_eq!(loaded.contexts.items[0].server, "https://first.example");
        assert_eq!(loaded.contexts.items[0].file.as_deref(), Some(a.as_path()));
        assert!(loaded.problems.is_empty());
    }

    #[test]
    fn context_details() {
        let t = TempDir::new("details");
        t.write(
            "a.yaml",
            &config(
                "skip",
                "c1",
                "https://one.example",
                "    insecure-skip-tls-verify: true\n    proxy-url: http://proxy.corp:3128\n",
                "    exec:\n      apiVersion: client.authentication.k8s.io/v1\n      command: /usr/local/bin/aws\n      args: [eks, get-token]\n      installHint: brew install awscli\n",
            ),
        );
        t.write("b.yaml", &config("plain", "c2", "http://10.0.0.1:8080", "", "    token: y\n"));
        t.write("c.yaml", &config("safe", "c3", "https://three.example", "", "    token: z\n"));
        let merged = read_merged(&KubeconfigSources::explicit([t.entry("*.yaml")]));
        let contexts = contexts_of(&merged.config, &merged.origin);
        let by = |n: &str| contexts.items.iter().find(|c| c.name == n).expect("context").clone();

        let skip = by("skip");
        assert!(skip.insecure);
        assert_eq!(skip.proxy.as_deref(), Some("http://proxy.corp:3128"));
        assert_eq!(skip.exec_command.as_deref(), Some("aws"));
        assert!(by("plain").insecure, "plain http is insecure");
        let safe = by("safe");
        assert!(!safe.insecure);
        assert_eq!((safe.proxy, safe.exec_command), (None, None));

        let exec = exec_of(&merged.config, "skip").expect("exec config");
        assert_eq!(exec.install_hint.as_deref(), Some("brew install awscli"));
        assert!(exec_of(&merged.config, "safe").is_none());
    }

    #[test]
    fn relative_certificate_paths_resolve_against_the_file() {
        let t = TempDir::new("rel");
        let file = t.write(
            "sub/a.yaml",
            &config("rel", "c1", "https://one.example", "    certificate-authority: ca.crt\n", "    token: x\n"),
        );
        let merged = read_merged(&KubeconfigSources::explicit([t.entry("sub/a.yaml")]));
        let ca = cluster_of(&merged.config, "c1").and_then(|c| c.certificate_authority.clone()).expect("ca");
        assert_eq!(PathBuf::from(ca), file.parent().expect("parent").join("ca.crt"));
    }

    #[test]
    fn directories_and_wildcards() {
        let t = TempDir::new("dir");
        t.write("d/2.yaml", &config("two", "c2", "https://two.example", "", "    token: x\n"));
        t.write("d/1.yaml", &config("one", "c1", "https://one.example", "", "    token: x\n"));
        t.write("d/.hidden", "junk");
        t.write("d/notes.txt", &config("txt", "c3", "https://three.example", "", "    token: x\n"));
        t.write("d/nested/3.yaml", &config("nested", "c4", "https://four.example", "", "    token: x\n"));

        let names =
            |s: &KubeconfigSources| -> Vec<String> { load(s).contexts.items.into_iter().map(|c| c.name).collect() };
        // A directory: every file directly in it, in name order.
        assert_eq!(names(&KubeconfigSources::explicit([t.entry("d")])), ["one", "two", "txt"]);
        // A wildcard in the last component.
        assert_eq!(names(&KubeconfigSources::explicit([t.entry("d/*.yaml")])), ["one", "two"]);
        assert_eq!(names(&KubeconfigSources::explicit([t.entry("d/?.yaml"), t.entry("d/1.yaml")])), ["one", "two"]);
        // Nothing matches: no contexts, no problems.
        let none = load(&KubeconfigSources::explicit([t.entry("d/*.json"), t.entry("nowhere/*")]));
        assert!(none.contexts.items.is_empty() && none.problems.is_empty() && none.files.is_empty());
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.yaml", "a.yaml"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("prod-*-eu.y?ml", "prod-a-b-eu.yaml"));
        assert!(wildcard_match("a*b*c", "aXbXbXc"));
        assert!(!wildcard_match("*.yaml", "a.yml"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(!wildcard_match("abc", "abcd"));
        assert!(wildcard_match("", ""));
    }

    #[test]
    fn tilde_and_default_paths() {
        let home = Path::new("/home/me");
        assert_eq!(expand_tilde("~/.kube/config", Some(home)), home.join(".kube/config"));
        assert_eq!(expand_tilde("~", Some(home)), home);
        // `~user` is not ours to expand, and neither is a tilde elsewhere.
        assert_eq!(expand_tilde("~other/x", Some(home)), PathBuf::from("~other/x"));
        assert_eq!(expand_tilde("/a/~/b", Some(home)), PathBuf::from("/a/~/b"));
        assert_eq!(expand_tilde("~/x", None), PathBuf::from("~/x"));

        let joined = std::env::join_paths([Path::new("/a/one"), Path::new(""), Path::new("/b/two")]).expect("join");
        assert_eq!(
            paths_from_env(Some(joined), Some(home.to_path_buf())),
            [PathBuf::from("/a/one"), PathBuf::from("/b/two")]
        );
        assert_eq!(paths_from_env(None, Some(home.to_path_buf())), [home.join(".kube").join("config")]);
        assert_eq!(
            paths_from_env(Some(OsString::new()), Some(home.to_path_buf())),
            [home.join(".kube").join("config")]
        );
        assert!(paths_from_env(None, None).is_empty());
    }

    #[test]
    fn sources_api() {
        assert!(KubeconfigSources::from_env().is_default());
        assert!(KubeconfigSources::default().is_default());
        let s = KubeconfigSources::explicit(["/x/a", "", "/x/b"]);
        assert_eq!(s.entries(), Some(&["/x/a".to_string(), "/x/b".to_string()][..]));
        let s = s.with_entry("/x/c").with_entry("/x/a");
        assert_eq!(s.resolve(), [PathBuf::from("/x/a"), PathBuf::from("/x/b"), PathBuf::from("/x/c")]);
        // An explicit empty list reads nothing at all.
        assert!(load(&KubeconfigSources::explicit(Vec::<String>::new())).searched.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn executables_are_found_on_path() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new("path");
        let tool = t.write("bin/my-plugin", "#!/bin/sh\n");
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        t.write("bin/not-exec", "data");
        let path = std::env::join_paths([PathBuf::from("/nonexistent-dir"), t.0.join("bin")]).expect("join");

        assert_eq!(find_executable_in("my-plugin", Some(path.clone())), Some(tool.clone()));
        assert_eq!(find_executable_in("not-exec", Some(path.clone())), None);
        assert_eq!(find_executable_in("absent", Some(path.clone())), None);
        assert_eq!(find_executable_in("my-plugin", None), None);
        // A path is checked as given, not searched.
        assert_eq!(find_executable_in(&tool.to_string_lossy(), None), Some(tool));
        assert_eq!(find_executable_in("/nonexistent-dir/my-plugin", Some(path)), None);
        assert_eq!(command_name("/usr/local/bin/aws"), "aws");
        assert_eq!(command_name("C:\\tools\\kubelogin.exe"), "kubelogin.exe");
        assert_eq!(command_name("gke-gcloud-auth-plugin"), "gke-gcloud-auth-plugin");
    }
}
