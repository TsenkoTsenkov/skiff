//! Resource identity and the curated registry of well-known kinds.

use std::sync::Arc;

/// Identifies one listable resource type on a cluster.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceKey {
    /// API group, empty for the core group.
    pub group: Arc<str>,
    pub version: Arc<str>,
    /// Plural resource name as used in URLs, e.g. `pods`.
    pub plural: Arc<str>,
    pub kind: Arc<str>,
    pub namespaced: bool,
}

impl ResourceKey {
    pub fn new(group: &str, version: &str, plural: &str, kind: &str, namespaced: bool) -> Self {
        Self {
            group: group.into(),
            version: version.into(),
            plural: plural.into(),
            kind: kind.into(),
            namespaced,
        }
    }

    pub fn api_version(&self) -> String {
        if self.group.is_empty() {
            self.version.to_string()
        } else {
            format!("{}/{}", self.group, self.version)
        }
    }

    fn base_path(&self) -> String {
        if self.group.is_empty() {
            format!("/api/{}", self.version)
        } else {
            format!("/apis/{}/{}", self.group, self.version)
        }
    }

    /// Collection URL. `ns` is ignored for cluster-scoped resources; an empty
    /// or missing namespace means "all namespaces".
    pub fn list_path(&self, ns: Option<&str>) -> String {
        match ns {
            Some(ns) if self.namespaced && !ns.is_empty() => {
                format!("{}/namespaces/{}/{}", self.base_path(), crate::util::enc_segment(ns), self.plural)
            }
            _ => format!("{}/{}", self.base_path(), self.plural),
        }
    }

    pub fn object_path(&self, ns: &str, name: &str) -> String {
        // RBAC object names may contain characters such as `#` or spaces
        // that would otherwise change the meaning of the URL.
        format!("{}/{}", self.list_path(Some(ns)), crate::util::enc_segment(name))
    }

    /// `pods`, `deployments.apps` — the form kubectl accepts.
    pub fn id(&self) -> String {
        if self.group.is_empty() {
            self.plural.to_string()
        } else {
            format!("{}.{}", self.plural, self.group)
        }
    }

    pub fn is(&self, group: &str, plural: &str) -> bool {
        &*self.group == group && &*self.plural == plural
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Cluster,
    Workloads,
    Config,
    Network,
    Storage,
    Access,
    Custom,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Cluster,
        Category::Workloads,
        Category::Config,
        Category::Network,
        Category::Storage,
        Category::Access,
        Category::Custom,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::Cluster => "Cluster",
            Category::Workloads => "Workloads",
            Category::Config => "Config",
            Category::Network => "Network",
            Category::Storage => "Storage",
            Category::Access => "Access Control",
            Category::Custom => "Custom Resources",
        }
    }
}

/// A well-known kind that gets a fixed place in the navigation.
#[derive(Clone, Copy, Debug)]
pub struct Builtin {
    pub group: &'static str,
    pub version: &'static str,
    pub plural: &'static str,
    pub kind: &'static str,
    pub namespaced: bool,
    pub title: &'static str,
    pub category: Category,
    /// Short aliases accepted by the command palette (kubectl short names).
    pub aliases: &'static [&'static str],
}

impl Builtin {
    pub fn key(&self) -> ResourceKey {
        ResourceKey::new(self.group, self.version, self.plural, self.kind, self.namespaced)
    }
}

macro_rules! b {
    ($cat:ident, $group:literal, $ver:literal, $plural:literal, $kind:literal, $ns:literal, $title:literal, [$($alias:literal),*]) => {
        Builtin {
            group: $group,
            version: $ver,
            plural: $plural,
            kind: $kind,
            namespaced: $ns,
            title: $title,
            category: Category::$cat,
            aliases: &[$($alias),*],
        }
    };
}

pub const BUILTINS: &[Builtin] = &[
    b!(Cluster, "", "v1", "nodes", "Node", false, "Nodes", ["no", "node"]),
    b!(Cluster, "", "v1", "namespaces", "Namespace", false, "Namespaces", ["ns"]),
    b!(Cluster, "", "v1", "events", "Event", true, "Events", ["ev"]),
    b!(Workloads, "", "v1", "pods", "Pod", true, "Pods", ["po", "pod"]),
    b!(Workloads, "apps", "v1", "deployments", "Deployment", true, "Deployments", ["deploy", "dp"]),
    b!(Workloads, "apps", "v1", "daemonsets", "DaemonSet", true, "DaemonSets", ["ds"]),
    b!(Workloads, "apps", "v1", "statefulsets", "StatefulSet", true, "StatefulSets", ["sts"]),
    b!(Workloads, "apps", "v1", "replicasets", "ReplicaSet", true, "ReplicaSets", ["rs"]),
    b!(Workloads, "batch", "v1", "jobs", "Job", true, "Jobs", ["job"]),
    b!(Workloads, "batch", "v1", "cronjobs", "CronJob", true, "CronJobs", ["cj"]),
    b!(Config, "", "v1", "configmaps", "ConfigMap", true, "ConfigMaps", ["cm"]),
    b!(Config, "", "v1", "secrets", "Secret", true, "Secrets", ["secret"]),
    b!(Config, "autoscaling", "v2", "horizontalpodautoscalers", "HorizontalPodAutoscaler", true, "HPAs", ["hpa"]),
    b!(Config, "policy", "v1", "poddisruptionbudgets", "PodDisruptionBudget", true, "Pod Disruption Budgets", ["pdb"]),
    b!(Config, "", "v1", "resourcequotas", "ResourceQuota", true, "Resource Quotas", ["quota"]),
    b!(Config, "", "v1", "limitranges", "LimitRange", true, "Limit Ranges", ["limits"]),
    b!(Config, "scheduling.k8s.io", "v1", "priorityclasses", "PriorityClass", false, "Priority Classes", ["pc"]),
    b!(Config, "coordination.k8s.io", "v1", "leases", "Lease", true, "Leases", []),
    b!(Network, "", "v1", "services", "Service", true, "Services", ["svc"]),
    b!(Network, "networking.k8s.io", "v1", "ingresses", "Ingress", true, "Ingresses", ["ing"]),
    b!(Network, "networking.k8s.io", "v1", "ingressclasses", "IngressClass", false, "Ingress Classes", []),
    b!(Network, "networking.k8s.io", "v1", "networkpolicies", "NetworkPolicy", true, "Network Policies", ["netpol"]),
    b!(Network, "discovery.k8s.io", "v1", "endpointslices", "EndpointSlice", true, "Endpoint Slices", ["eps"]),
    b!(Storage, "", "v1", "persistentvolumeclaims", "PersistentVolumeClaim", true, "Persistent Volume Claims", ["pvc"]),
    b!(Storage, "", "v1", "persistentvolumes", "PersistentVolume", false, "Persistent Volumes", ["pv"]),
    b!(Storage, "storage.k8s.io", "v1", "storageclasses", "StorageClass", false, "Storage Classes", ["sc"]),
    b!(Access, "", "v1", "serviceaccounts", "ServiceAccount", true, "Service Accounts", ["sa"]),
    b!(Access, "rbac.authorization.k8s.io", "v1", "roles", "Role", true, "Roles", []),
    b!(Access, "rbac.authorization.k8s.io", "v1", "rolebindings", "RoleBinding", true, "Role Bindings", ["rb"]),
    b!(Access, "rbac.authorization.k8s.io", "v1", "clusterroles", "ClusterRole", false, "Cluster Roles", ["cr"]),
    b!(Access, "rbac.authorization.k8s.io", "v1", "clusterrolebindings", "ClusterRoleBinding", false, "Cluster Role Bindings", ["crb"]),
    b!(Custom, "apiextensions.k8s.io", "v1", "customresourcedefinitions", "CustomResourceDefinition", false, "Definitions", ["crd", "crds"]),
];

pub fn builtin(group: &str, plural: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.group == group && b.plural == plural)
}

pub fn builtin_by_kind(kind: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.kind == kind)
}

pub fn pods() -> ResourceKey {
    builtin("", "pods").unwrap().key()
}
pub fn nodes() -> ResourceKey {
    builtin("", "nodes").unwrap().key()
}
pub fn namespaces() -> ResourceKey {
    builtin("", "namespaces").unwrap().key()
}
pub fn events() -> ResourceKey {
    builtin("", "events").unwrap().key()
}

/// A resource type found through API discovery.
#[derive(Clone, Debug)]
pub struct Discovered {
    pub key: ResourceKey,
    pub verbs: Vec<String>,
}

impl Discovered {
    pub fn can(&self, verb: &str) -> bool {
        self.verbs.iter().any(|v| v == verb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let p = pods();
        assert_eq!(p.list_path(None), "/api/v1/pods");
        assert_eq!(p.list_path(Some("")), "/api/v1/pods");
        assert_eq!(p.list_path(Some("kube-system")), "/api/v1/namespaces/kube-system/pods");
        assert_eq!(p.object_path("default", "web-0"), "/api/v1/namespaces/default/pods/web-0");
        let d = builtin("apps", "deployments").unwrap().key();
        assert_eq!(d.list_path(None), "/apis/apps/v1/deployments");
        assert_eq!(d.id(), "deployments.apps");
        let n = nodes();
        assert_eq!(n.object_path("", "n1"), "/api/v1/nodes/n1");
        assert_eq!(n.list_path(Some("ignored")), "/api/v1/nodes");
        assert_eq!(p.list_path(Some("a/b?c")), "/api/v1/namespaces/a%2Fb%3Fc/pods");
        let cr = builtin("rbac.authorization.k8s.io", "clusterroles").unwrap().key();
        assert_eq!(
            cr.object_path("", "system:a#b c"),
            "/apis/rbac.authorization.k8s.io/v1/clusterroles/system:a%23b%20c"
        );
    }

    #[test]
    fn registry_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for b in BUILTINS {
            assert!(seen.insert((b.group, b.plural)), "duplicate {}", b.plural);
        }
    }
}
