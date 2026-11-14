//! One-shot operations on objects: fetch, edit, delete, scale, restart …

use http::Method;
use kube::Client;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::kinds::{Discovered, ResourceKey};
use crate::util::{friendly, get_request, request};

const MERGE_PATCH: &str = "application/merge-patch+json";
const APPLY_PATCH: &str = "application/apply-patch+yaml";
const JSON: &str = "application/json";

/// A fetched object in the forms the UI needs.
#[derive(Clone, Debug)]
pub struct ObjectData {
    pub value: Value,
    /// YAML with `managedFields` removed.
    pub yaml: String,
    pub rv: String,
}

fn to_object_data(mut value: Value) -> Result<ObjectData, String> {
    if let Some(meta) = value.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        meta.remove("managedFields");
    }
    let rv = value
        .pointer("/metadata/resourceVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let yaml = serde_yaml::to_string(&value).map_err(|e| format!("cannot render YAML: {e}"))?;
    Ok(ObjectData { value, yaml, rv })
}

async fn send(client: &Client, req: http::Request<Vec<u8>>) -> Result<Value, String> {
    client.request::<Value>(req).await.map_err(|e| friendly(&e))
}

pub async fn get_object(client: &Client, key: &ResourceKey, ns: &str, name: &str) -> Result<ObjectData, String> {
    let req = get_request(&key.object_path(ns, name), None)?;
    to_object_data(send(client, req).await?)
}

/// Replaces an object with edited YAML. The YAML's `resourceVersion` makes
/// this fail with a conflict if the object changed in the meantime.
pub async fn replace_object(
    client: &Client,
    key: &ResourceKey,
    ns: &str,
    name: &str,
    yaml: &str,
) -> Result<ObjectData, String> {
    let value: Value = serde_yaml::from_str(yaml).map_err(|e| format!("invalid YAML: {e}"))?;
    let edited_name = value.pointer("/metadata/name").and_then(|v| v.as_str()).unwrap_or("");
    if edited_name != name {
        return Err(format!("metadata.name cannot be changed (was {name:?}, now {edited_name:?})"));
    }
    let body = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    let req = request(Method::PUT, &key.object_path(ns, name), body, Some(JSON), None)?;
    to_object_data(send(client, req).await?)
}

pub async fn delete_object(
    client: &Client,
    key: &ResourceKey,
    ns: &str,
    name: &str,
    force: bool,
) -> Result<(), String> {
    let mut opts = json!({"propagationPolicy": "Background"});
    if force {
        opts["gracePeriodSeconds"] = json!(0);
    }
    let req = request(
        Method::DELETE,
        &key.object_path(ns, name),
        serde_json::to_vec(&opts).unwrap_or_default(),
        Some(JSON),
        None,
    )?;
    send(client, req).await.map(|_| ())
}

pub async fn merge_patch(client: &Client, path: &str, patch: &Value) -> Result<(), String> {
    let req = request(
        Method::PATCH,
        path,
        serde_json::to_vec(patch).map_err(|e| e.to_string())?,
        Some(MERGE_PATCH),
        None,
    )?;
    send(client, req).await.map(|_| ())
}

pub async fn scale(client: &Client, key: &ResourceKey, ns: &str, name: &str, replicas: i32) -> Result<(), String> {
    let path = format!("{}/scale", key.object_path(ns, name));
    merge_patch(client, &path, &json!({"spec": {"replicas": replicas}})).await
}

/// `kubectl rollout restart`: bumps a pod-template annotation.
pub async fn restart(client: &Client, key: &ResourceKey, ns: &str, name: &str) -> Result<(), String> {
    let now = jiff::Timestamp::now().to_string();
    let patch = json!({"spec": {"template": {"metadata": {"annotations": {
        "kubectl.kubernetes.io/restartedAt": now
    }}}}});
    merge_patch(client, &key.object_path(ns, name), &patch).await
}

pub async fn cordon(client: &Client, node: &str, unschedulable: bool) -> Result<(), String> {
    let key = crate::kinds::nodes();
    let value = if unschedulable { json!(true) } else { Value::Null };
    merge_patch(client, &key.object_path("", node), &json!({"spec": {"unschedulable": value}})).await
}

pub async fn suspend_cronjob(client: &Client, key: &ResourceKey, ns: &str, name: &str, suspend: bool) -> Result<(), String> {
    merge_patch(client, &key.object_path(ns, name), &json!({"spec": {"suspend": suspend}})).await
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    pub init: bool,
    /// Declared container ports as (name, port).
    pub ports: Vec<(String, u16)>,
}

/// Containers of a pod, main containers first.
pub fn containers_of(pod: &Value) -> Vec<ContainerInfo> {
    let mut out = Vec::new();
    for (field, init) in [("containers", false), ("initContainers", true), ("ephemeralContainers", false)] {
        let Some(list) = pod.pointer(&format!("/spec/{field}")).and_then(|v| v.as_array()) else { continue };
        for c in list {
            let ports = c
                .get("ports")
                .and_then(|p| p.as_array())
                .map(|ports| {
                    ports
                        .iter()
                        .filter_map(|p| {
                            let port = p.get("containerPort")?.as_u64()? as u16;
                            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                            Some((name, port))
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(ContainerInfo {
                name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                image: c.get("image").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                init,
                ports,
            });
        }
    }
    out
}

pub async fn pod_containers(client: &Client, ns: &str, pod: &str) -> Result<Vec<ContainerInfo>, String> {
    let key = crate::kinds::pods();
    let req = get_request(&key.object_path(ns, pod), None)?;
    Ok(containers_of(&send(client, req).await?))
}

/// Renders a label selector map as `k=v,k2=v2`.
pub fn selector_string(selector: &Value) -> Option<String> {
    let labels = selector.get("matchLabels").unwrap_or(selector).as_object()?;
    if labels.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = labels
        .iter()
        .filter_map(|(k, v)| Some(format!("{k}={}", v.as_str()?)))
        .collect();
    if let Some(exprs) = selector.get("matchExpressions").and_then(|e| e.as_array()) {
        for e in exprs {
            let key = e.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let values: Vec<&str> = e
                .get("values")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            match e.get("operator").and_then(|v| v.as_str()).unwrap_or("") {
                "In" => parts.push(format!("{key} in ({})", values.join(","))),
                "NotIn" => parts.push(format!("{key} notin ({})", values.join(","))),
                "Exists" => parts.push(key.to_string()),
                "DoesNotExist" => parts.push(format!("!{key}")),
                _ => {}
            }
        }
    }
    Some(parts.join(","))
}

/// The label selector that finds the pods of a workload or service object.
pub fn pod_selector_of(obj: &Value) -> Option<String> {
    let kind = obj.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let sel = obj.pointer("/spec/selector")?;
    match kind {
        // Service selectors are a plain map.
        "Service" | "ReplicationController" => selector_string(sel),
        _ => selector_string(sel),
    }
}

#[derive(Deserialize)]
struct PodListLite {
    #[serde(default)]
    items: Vec<Value>,
}

fn pod_is_ready(pod: &Value) -> bool {
    pod.pointer("/status/conditions")
        .and_then(|c| c.as_array())
        .map(|conds| {
            conds.iter().any(|c| {
                c.get("type").and_then(|t| t.as_str()) == Some("Ready")
                    && c.get("status").and_then(|s| s.as_str()) == Some("True")
            })
        })
        .unwrap_or(false)
}

/// Picks a ready pod behind a service and resolves the service port to the
/// container port to forward to.
pub async fn resolve_service(client: &Client, ns: &str, service: &str, port: u16) -> Result<(String, u16), String> {
    let svc_key = crate::kinds::builtin("", "services").unwrap().key();
    let svc = send(client, get_request(&svc_key.object_path(ns, service), None)?).await?;
    let selector = pod_selector_of(&svc).ok_or("service has no selector")?;
    let target = svc
        .pointer("/spec/ports")
        .and_then(|p| p.as_array())
        .and_then(|ports| {
            ports
                .iter()
                .find(|p| p.get("port").and_then(|v| v.as_u64()) == Some(port as u64))
        })
        .and_then(|p| p.get("targetPort").cloned())
        .unwrap_or(json!(port));

    let url = format!(
        "{}?labelSelector={}",
        crate::kinds::pods().list_path(Some(ns)),
        crate::util::enc(&selector)
    );
    let pods: PodListLite = client
        .request(get_request(&url, None)?)
        .await
        .map_err(|e| friendly(&e))?;
    let pod = pods
        .items
        .iter()
        .find(|p| pod_is_ready(p))
        .or(pods.items.first())
        .ok_or("no pods match the service selector")?;
    let pod_name = pod.pointer("/metadata/name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let remote = match &target {
        Value::Number(n) => n.as_u64().unwrap_or(port as u64) as u16,
        Value::String(name) => containers_of(pod)
            .iter()
            .flat_map(|c| c.ports.iter())
            .find(|(n, _)| n == name)
            .map(|(_, p)| *p)
            .ok_or_else(|| format!("named port {name:?} not found on pod {pod_name}"))?,
        _ => port,
    };
    Ok((pod_name, remote))
}

/// Ports that can be forwarded to on a pod (declared container ports) or a
/// service (its service ports), as (name, port).
pub async fn forwardable_ports(client: &Client, service: bool, ns: &str, name: &str) -> Result<Vec<(String, u16)>, String> {
    if service {
        let key = crate::kinds::builtin("", "services").unwrap().key();
        let svc = send(client, get_request(&key.object_path(ns, name), None)?).await?;
        Ok(svc
            .pointer("/spec/ports")
            .and_then(|p| p.as_array())
            .map(|ports| {
                ports
                    .iter()
                    .filter_map(|p| {
                        let port = p.get("port")?.as_u64()? as u16;
                        let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                        Some((name, port))
                    })
                    .collect()
            })
            .unwrap_or_default())
    } else {
        Ok(pod_containers(client, ns, name)
            .await?
            .into_iter()
            .flat_map(|c| c.ports.into_iter())
            .collect())
    }
}

/// Containers to stream logs from for a pod or a workload (all pods matched
/// by its selector). At most `MAX_LOG_PODS` pods are followed.
pub async fn log_targets(
    client: &Client,
    key: &ResourceKey,
    ns: &str,
    name: &str,
) -> Result<Vec<crate::logs::LogTarget>, String> {
    const MAX_LOG_PODS: usize = 20;
    let pods_key = crate::kinds::pods();
    let pods: Vec<Value> = if key.is("", "pods") {
        vec![send(client, get_request(&pods_key.object_path(ns, name), None)?).await?]
    } else {
        let obj = send(client, get_request(&key.object_path(ns, name), None)?).await?;
        let selector = pod_selector_of(&obj).ok_or("this object has no pod selector")?;
        let url = format!(
            "{}?labelSelector={}&limit={}",
            pods_key.list_path(Some(ns)),
            crate::util::enc(&selector),
            MAX_LOG_PODS
        );
        let list: PodListLite = client.request(get_request(&url, None)?).await.map_err(|e| friendly(&e))?;
        list.items
    };
    let mut targets = Vec::new();
    for pod in &pods {
        let pod_name = pod.pointer("/metadata/name").and_then(|v| v.as_str()).unwrap_or("");
        for c in containers_of(pod).into_iter().filter(|c| !c.init) {
            targets.push(crate::logs::LogTarget {
                namespace: ns.to_string(),
                pod: pod_name.to_string(),
                container: c.name,
            });
        }
    }
    if targets.is_empty() {
        return Err("no pods found".into());
    }
    Ok(targets)
}

/// How long the API server may take over one request before it counts as
/// not answering.
pub(crate) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Sends a request and returns the HTTP status alongside the parsed body.
async fn send_with_status(client: &Client, req: http::Request<Vec<u8>>) -> Result<(http::StatusCode, Value), String> {
    let res = client
        .send(req.map(kube::client::Body::from))
        .await
        .map_err(|e| friendly(&e))?;
    let status = res.status();
    let bytes = res.into_body().collect_bytes().await.map_err(|e| friendly(&e))?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        Ok((status, value))
    } else {
        let msg = value.get("message").and_then(|m| m.as_str()).unwrap_or("request failed");
        Err(format!("{msg} ({})", status.as_u16()))
    }
}

/// `kubectl apply --server-side` for every document in a (multi-document)
/// manifest: fields in the manifest are set and owned by Skiff, everything
/// else on an existing object is left alone.
///
/// Every document is attempted. `Ok` lists what happened to each; if any
/// document failed, `Err` carries the same per-document report including
/// the failures. Each request has its own time limit, so a large manifest on
/// a slow connection is not cut off half-way; only when the server stops
/// answering are the remaining documents left alone, and the report says so.
pub async fn apply_manifest(client: &Client, discovered: &[Discovered], yaml: &str) -> Result<Vec<String>, String> {
    // Parse everything up front: the YAML parser is not `Send` and must not
    // live across an await.
    let mut docs = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(yaml) {
        let value = Value::deserialize(doc).map_err(|e| format!("invalid YAML: {e}"))?;
        if value.is_null() {
            continue;
        }
        // `kubectl get -o yaml` of several objects produces a List.
        let is_list = value.get("kind").and_then(|k| k.as_str()).is_some_and(|k| k.ends_with("List"))
            && value.get("items").is_some_and(|i| i.is_array());
        match value {
            Value::Object(mut map) if is_list => {
                if let Some(Value::Array(items)) = map.remove("items") {
                    docs.extend(items.into_iter().filter(|i| !i.is_null()));
                }
            }
            other => docs.push(other),
        }
    }
    if docs.is_empty() {
        return Err("manifest is empty".into());
    }
    let mut report = Vec::new();
    let mut failed = false;
    let total = docs.len();
    for (index, value) in docs.into_iter().enumerate() {
        let api_version = value.get("apiVersion").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let name = value.pointer("/metadata/name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if api_version.is_empty() || kind.is_empty() || name.is_empty() {
            report.push("skipped a document without apiVersion, kind and metadata.name".to_string());
            failed = true;
            continue;
        }
        // Match on group and kind, then address the object at the version
        // the manifest asks for: discovery only lists each group's preferred
        // version, but the server serves others too (autoscaling/v1, ...).
        let (group, version) = match api_version.split_once('/') {
            Some((g, v)) => (g, v),
            None => ("", api_version.as_str()),
        };
        let key = discovered
            .iter()
            .map(|d| &d.key)
            .find(|k| *k.kind == *kind && *k.group == *group)
            .cloned()
            .or_else(|| crate::kinds::builtin_by_kind(&kind).map(|b| b.key()).filter(|k| &*k.group == group))
            .map(|k| ResourceKey::new(group, version, &k.plural, &k.kind, k.namespaced));
        let Some(key) = key else {
            report.push(format!("{kind}/{name}: unknown resource type {api_version}/{kind}"));
            failed = true;
            continue;
        };
        // Like kubectl: an object without a namespace goes to the namespace
        // of the kubeconfig context.
        let ns = if key.namespaced {
            value
                .pointer("/metadata/namespace")
                .and_then(|v| v.as_str())
                .filter(|n| !n.is_empty())
                .unwrap_or(client.default_namespace())
                .to_string()
        } else {
            String::new()
        };
        let label = format!("{}/{}", key.id(), name);
        let url = format!("{}?fieldManager=skiff&force=true", key.object_path(&ns, &name));
        let body = match serde_json::to_vec(&value) {
            Ok(b) => b,
            Err(e) => {
                report.push(format!("{label}: {e}"));
                failed = true;
                continue;
            }
        };
        let outcome = match request(Method::PATCH, &url, body, Some(APPLY_PATCH), None) {
            Ok(req) => match tokio::time::timeout(REQUEST_TIMEOUT, send_with_status(client, req)).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    report.push(format!(
                        "{label}: no response from the API server within {} s",
                        REQUEST_TIMEOUT.as_secs()
                    ));
                    let rest = total - index - 1;
                    if rest > 0 {
                        report.push(format!("{rest} more {} not attempted", if rest == 1 { "document" } else { "documents" }));
                    }
                    failed = true;
                    break;
                }
            },
            Err(e) => Err(e),
        };
        match outcome {
            Ok((status, _)) if status == http::StatusCode::CREATED => report.push(format!("{label} created")),
            Ok(_) => report.push(format!("{label} configured")),
            Err(e) => {
                report.push(format!("{label}: {e}"));
                failed = true;
            }
        }
    }
    if failed {
        Err(report.join("\n"))
    } else {
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_strips_managed_fields() {
        let v = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"a","resourceVersion":"7","managedFields":[{"manager":"kubectl"}]},"data":{"k":"line1\nline2\n"}});
        let d = to_object_data(v).unwrap();
        assert_eq!(d.rv, "7");
        assert!(!d.yaml.contains("managedFields"));
        assert!(d.yaml.contains("kind: ConfigMap"));
        // Multi-line strings stay readable.
        assert!(d.yaml.contains("k: |"), "{}", d.yaml);
        let back: Value = serde_yaml::from_str(&d.yaml).unwrap();
        assert_eq!(back["data"]["k"], "line1\nline2\n");
    }

    #[test]
    fn selectors() {
        let dep = json!({"kind":"Deployment","spec":{"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"tier","operator":"In","values":["a","b"]}]}}});
        assert_eq!(pod_selector_of(&dep).unwrap(), "app=web,tier in (a,b)");
        let svc = json!({"kind":"Service","spec":{"selector":{"app":"web","role":"api"}}});
        assert_eq!(pod_selector_of(&svc).unwrap(), "app=web,role=api");
        let headless = json!({"kind":"Service","spec":{}});
        assert!(pod_selector_of(&headless).is_none());
    }

    #[test]
    fn containers() {
        let pod = json!({"spec":{"initContainers":[{"name":"init","image":"busybox"}],"containers":[{"name":"app","image":"nginx:1","ports":[{"name":"http","containerPort":8080}]}]}});
        let c = containers_of(&pod);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "app");
        assert!(!c[0].init);
        assert_eq!(c[0].ports, vec![("http".to_string(), 8080)]);
        assert!(c[1].init);
    }
}
