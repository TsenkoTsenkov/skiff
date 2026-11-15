//! Which kubeconfig files the app reads.
//!
//! By default the same files as `kubectl`: those in `$KUBECONFIG`, else
//! `~/.kube/config`. The user can replace that with a list of their own
//! (Settings), which then applies to everything: the cluster list, new
//! connections and `kubectl` commands opened in a terminal app.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use kube::config::Kubeconfig;

static OVERRIDE: RwLock<Option<Vec<PathBuf>>> = RwLock::new(None);

/// Uses exactly these files from now on (`None`: back to `kubectl`'s
/// choice). Clusters already connected keep their connection until they
/// reconnect.
pub fn set_kubeconfig_paths(paths: Option<Vec<PathBuf>>) {
    let paths = paths.map(|v| v.into_iter().map(|p| expand_home(&p)).collect::<Vec<_>>());
    *OVERRIDE.write().unwrap_or_else(|e| e.into_inner()) = paths;
}

/// The files chosen with [`set_kubeconfig_paths`], if any.
pub fn kubeconfig_override() -> Option<Vec<PathBuf>> {
    OVERRIDE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The files `kubectl` would read: `$KUBECONFIG`, else `~/.kube/config`.
pub fn default_kubeconfig_paths() -> Vec<PathBuf> {
    match std::env::var_os("KUBECONFIG") {
        Some(v) if !v.is_empty() => std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect(),
        _ => home_dir().map(|h| vec![h.join(".kube").join("config")]).unwrap_or_default(),
    }
}

/// The files in effect.
pub fn kubeconfig_paths() -> Vec<PathBuf> {
    kubeconfig_override().unwrap_or_else(default_kubeconfig_paths)
}

/// `~/…` and `$HOME/…` to an absolute path.
pub fn expand_home(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    for prefix in ["~/", "$HOME/"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            if let Some(home) = home_dir() {
                return home.join(rest);
            }
        }
    }
    if text == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    path.to_path_buf()
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// What one kubeconfig file holds, for the settings page.
#[derive(Clone, Debug)]
pub struct FileReport {
    pub path: PathBuf,
    /// Context names, or why the file could not be read.
    pub contexts: Result<Vec<String>, String>,
}

/// Reads each file on its own (to say which one is broken).
pub fn inspect(paths: &[PathBuf]) -> Vec<FileReport> {
    paths
        .iter()
        .map(|p| FileReport {
            path: p.clone(),
            contexts: Kubeconfig::read_from(p)
                .map(|kc| kc.contexts.iter().map(|c| c.name.clone()).collect())
                .map_err(|e| e.to_string()),
        })
        .collect()
}

/// Reads and merges the files in effect, like `kubectl` does (the first file
/// that defines a name wins). Files that cannot be read are skipped and
/// reported in the error text, as long as at least one could be read.
pub fn read_kubeconfig() -> Result<(Kubeconfig, Option<String>), String> {
    let Some(paths) = kubeconfig_override() else {
        return Kubeconfig::read().map(|kc| (kc, None)).map_err(|e| format!("cannot read kubeconfig: {e}"));
    };
    if paths.is_empty() {
        return Err("no kubeconfig files are configured (Settings)".into());
    }
    let mut merged: Option<Kubeconfig> = None;
    let mut problems = Vec::new();
    for p in &paths {
        match Kubeconfig::read_from(p) {
            Ok(kc) => {
                merged = Some(match merged {
                    None => kc,
                    Some(m) => match m.clone().merge(kc) {
                        Ok(m) => m,
                        Err(e) => {
                            problems.push(format!("{}: {e}", p.display()));
                            m
                        }
                    },
                })
            }
            Err(e) => problems.push(format!("{}: {e}", p.display())),
        }
    }
    let problems = (!problems.is_empty()).then(|| problems.join("\n"));
    match merged {
        Some(kc) => Ok((kc, problems)),
        None => Err(format!("cannot read kubeconfig: {}", problems.unwrap_or_default())),
    }
}

/// The value for a `KUBECONFIG=` variable that makes `kubectl` see the same
/// files, when the user chose them (otherwise `kubectl`'s own lookup already
/// agrees and nothing is needed).
pub fn kubeconfig_env_value() -> Option<String> {
    let paths = kubeconfig_override()?;
    std::env::join_paths(paths).ok().map(|v| v.to_string_lossy().into_owned())
}

/// Files under `~/.kube` that look like kubeconfigs and are not in use, to
/// offer on the settings page.
pub fn suggestions() -> Vec<PathBuf> {
    let Some(dir) = home_dir().map(|h| h.join(".kube")) else { return Vec::new() };
    let in_use = kubeconfig_paths();
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && !in_use.contains(p))
        .filter(|p| {
            // Small YAML files that mention clusters and contexts.
            std::fs::metadata(p).map(|m| m.len() < 4 << 20).unwrap_or(false)
                && std::fs::read_to_string(p)
                    .map(|t| t.contains("clusters:") && t.contains("contexts:"))
                    .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_expanded() {
        let Some(home) = home_dir() else { return };
        assert_eq!(expand_home(Path::new("~/.kube/config")), home.join(".kube/config"));
        assert_eq!(expand_home(Path::new("/etc/kube")), PathBuf::from("/etc/kube"));
    }
}
