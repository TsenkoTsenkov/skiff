#!/usr/bin/env python3
"""A stand-in for Prometheus on the kwok "demo" cluster (stdlib only).

kwok pods are not real, so nothing can scrape them. This serves the three
endpoints Skiff uses, with synthetic data derived from the query text and
from the objects that really exist in the cluster (read with kubectl):

  /api/v1/query        instant queries (only what Skiff sends)
  /api/v1/query_range  per-container / per-pod / per-node series
  /api/v1/alerts       a handful of alerts about objects in the demo seed

The data is deterministic: the same query over the same time range gives the
same curve, so screenshots are stable. Two extra path prefixes exist for
testing error handling: /deny/... answers 403 and /broken/... answers 500.

  python3 -I fake-prometheus.py PORT [--bind ADDRESS] [--kubeconfig FILE] [--context NAME]
  python3 -I fake-prometheus.py --print-address   # this host's non-loopback IPv4
"""
import hashlib
import http.server
import json
import math
import re
import socket
import socketserver
import subprocess
import sys
import threading
import time
import urllib.parse


def host_address():
    """A non-loopback IPv4 address of this host, or "" when there is none.

    The API server refuses loopback and link-local addresses in Endpoints,
    so the Service that points at this process needs a "real" one.
    """
    candidates = []
    try:
        # Connecting a UDP socket sends nothing; it only asks the kernel
        # which source address it would use.
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("192.0.2.1", 9))
        candidates.append(s.getsockname()[0])
        s.close()
    except OSError:
        pass
    try:
        candidates += [a[4][0] for a in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET)]
    except OSError:
        pass
    try:
        out = subprocess.run(["hostname", "-I"], capture_output=True, text=True, timeout=5).stdout
        candidates += out.split()
    except (OSError, subprocess.SubprocessError):
        pass
    for ip in candidates:
        if ":" in ip or ip.startswith(("127.", "169.254.", "0.")):
            continue
        return ip
    return ""


# ---------------------------------------------------------------------------
# What exists in the cluster


def quantity(text):
    """Parses a Kubernetes quantity ("250m", "512Mi", "2") into a float."""
    if text is None:
        return None
    text = str(text)
    units = {"n": 1e-9, "u": 1e-6, "m": 1e-3, "k": 1e3, "M": 1e6, "G": 1e9, "T": 1e12,
             "Ki": 2 ** 10, "Mi": 2 ** 20, "Gi": 2 ** 30, "Ti": 2 ** 40}
    for suffix in sorted(units, key=len, reverse=True):
        if text.endswith(suffix):
            try:
                return float(text[: -len(suffix)]) * units[suffix]
            except ValueError:
                return None
    try:
        return float(text)
    except ValueError:
        return None


class Cluster:
    """Pods and nodes, read with kubectl and cached."""

    def __init__(self, kubeconfig, context):
        self.kubeconfig, self.context = kubeconfig, context
        self.lock = threading.Lock()
        self.loaded_at = 0.0
        self.pods = {}    # (namespace, name) -> {"containers": [...], "crashloop": bool, "node": str}
        self.nodes = {}   # name -> {"cpu": cores, "memory": bytes}

    def kubectl(self, *args):
        cmd = ["kubectl"]
        if self.kubeconfig:
            cmd += ["--kubeconfig", self.kubeconfig]
        if self.context:
            cmd += ["--context", self.context]
        out = subprocess.run(cmd + list(args) + ["-o", "json", "--request-timeout=10s"],
                             capture_output=True, text=True, timeout=20)
        return json.loads(out.stdout) if out.returncode == 0 else None

    def refresh(self, max_age):
        with self.lock:
            if time.time() - self.loaded_at < max_age:
                return
            self.loaded_at = time.time()
            try:
                pods, nodes = self.kubectl("get", "pods", "-A"), self.kubectl("get", "nodes")
            except (OSError, ValueError, subprocess.SubprocessError):
                return
            if pods is not None:
                found = {}
                for p in pods.get("items", []):
                    meta, spec, status = p.get("metadata", {}), p.get("spec", {}), p.get("status", {})
                    crash = any(
                        (st.get("state", {}).get("waiting") or {}).get("reason") == "CrashLoopBackOff"
                        for st in status.get("containerStatuses", []))
                    containers = []
                    for c in spec.get("containers", []):
                        res = c.get("resources", {})
                        req, lim = res.get("requests", {}), res.get("limits", {})
                        containers.append({
                            "name": c.get("name", "app"),
                            "cpu_request": quantity(req.get("cpu")), "cpu_limit": quantity(lim.get("cpu")),
                            "memory_request": quantity(req.get("memory")), "memory_limit": quantity(lim.get("memory")),
                        })
                    found[(meta.get("namespace", ""), meta.get("name", ""))] = {
                        "containers": containers, "crashloop": crash, "node": spec.get("nodeName", "")}
                self.pods = found
            if nodes is not None:
                self.nodes = {
                    n["metadata"]["name"]: {
                        "cpu": quantity(n.get("status", {}).get("allocatable", {}).get("cpu")) or 4.0,
                        "memory": quantity(n.get("status", {}).get("allocatable", {}).get("memory")) or 16 * 2 ** 30,
                    }
                    for n in nodes.get("items", [])
                }

    def pod(self, namespace, name):
        self.refresh(300)
        if (namespace, name) not in self.pods:
            self.refresh(15)   # a pod that appeared since the last look
        return self.pods.get((namespace, name)) or {
            "containers": [{"name": "app", "cpu_request": None, "cpu_limit": None,
                            "memory_request": None, "memory_limit": None}],
            "crashloop": False, "node": ""}

    def node(self, name):
        self.refresh(300)
        return self.nodes.get(name) or {"cpu": 8.0, "memory": 32 * 2 ** 30}

    def first_pod(self, namespace, prefix):
        self.refresh(300)
        names = sorted(n for (ns, n) in self.pods if ns == namespace and n.startswith(prefix))
        return names[0] if names else None


# ---------------------------------------------------------------------------
# Synthetic curves


def unit(*key):
    """A stable number in [0, 1) for a key."""
    digest = hashlib.blake2b("\x00".join(str(k) for k in key).encode(), digest_size=8).digest()
    return int.from_bytes(digest, "big") / 2 ** 64


def noise(key, t, period):
    """Smooth value noise in [0, 1): random levels every `period` seconds, eased between."""
    i, frac = divmod(t / period, 1.0)
    a, b = unit(key, int(i)), unit(key, int(i) + 1)
    ease = frac * frac * (3 - 2 * frac)
    return a + (b - a) * ease


def daily(key, t):
    """A day/night rhythm around 1.0."""
    return 1.0 + 0.22 * math.sin(2 * math.pi * (t / 86400.0 + unit(key, "phase")))


def cpu_cores(pod, c, crashloop, t):
    key = (pod, c["name"], "cpu")
    request, limit = c["cpu_request"], c["cpu_limit"]
    if crashloop:
        # Starts, burns a little CPU, dies: a comb of short bursts.
        phase = (t + 600 * unit(key, "offset")) % 330
        return 0.004 + (0.06 if phase < 40 else 0.0) * (0.5 + unit(key, int(t // 330)))
    base = request * (0.35 + 0.5 * unit(key)) if request else 0.015 + 0.12 * unit(key)
    v = base * daily(key, t) * (0.7 + 0.6 * noise(key, t, 420)) * (0.92 + 0.16 * noise(key + ("fast",), t, 45))
    burst = noise(key + ("burst",), t, 2700)
    if burst > 0.8:
        v *= 1.0 + 4.0 * (burst - 0.8)
    return min(v, limit) if limit else v


def memory_bytes(pod, c, crashloop, t):
    key = (pod, c["name"], "mem")
    request, limit = c["memory_request"], c["memory_limit"]
    top = limit or (request * 1.6 if request else (48 + 400 * unit(key)) * 2 ** 20)
    if crashloop:
        phase = (t + 600 * unit(key, "offset")) % 330
        return top * (0.08 + 0.5 * min(phase, 40) / 40.0 * (1 if phase < 40 else 0))
    # Slow growth and a drop, like a heap between collections.
    period = 3600 * (2 + 5 * unit(key, "period"))
    saw = ((t + period * unit(key, "offset")) % period) / period
    level = 0.42 + 0.2 * unit(key) + 0.16 * saw + 0.03 * noise(key, t, 240)
    return top * min(level, 0.97)


def network_bytes(pod, direction, t):
    key = (pod, direction)
    base = (20 + 900 * unit(key) ** 2) * 1024 * (1.0 if direction == "rx" else 0.6)
    return base * daily(key, t) * (0.5 + noise(key, t, 300)) * (0.9 + 0.2 * noise(key + ("fast",), t, 40))


def restarts(pod, crashloop, t0, t1):
    """Restarts in (t0, t1]."""
    if not crashloop:
        return 0.0
    period = 330.0
    offset = 600 * unit(pod, "cpu-offset")
    return float(max(0, math.floor((t1 + offset) / period) - math.floor((t0 + offset) / period)))


def node_cpu(name, alloc, t):
    key = (name, "node-cpu")
    return alloc * (0.22 + 0.38 * unit(key)) * daily(key, t) * (0.8 + 0.4 * noise(key, t, 600)) \
        * (0.95 + 0.1 * noise(key + ("fast",), t, 60))


def node_memory(name, alloc, t):
    key = (name, "node-mem")
    return alloc * (0.35 + 0.35 * unit(key) + 0.06 * noise(key, t, 1800) + 0.01 * noise(key + ("fast",), t, 120))


# ---------------------------------------------------------------------------
# Queries

METRICS = (
    "container_cpu_usage_seconds_total", "container_memory_working_set_bytes",
    "container_network_receive_bytes_total", "container_network_transmit_bytes_total",
    "kube_pod_container_status_restarts_total", "kube_pod_container_resource_requests",
    "kube_pod_container_resource_limits", "kube_node_status_allocatable",
)


def unquote(value):
    """Undoes PromQL string escaping."""
    return re.sub(r"\\(.)", r"\1", value)


def matchers(query):
    """label -> (operator, value) for every matcher in the query."""
    out = {}
    for label, op, value in re.findall(r'([a-zA-Z_][a-zA-Z0-9_]*)\s*(=~|!=|=)\s*"((?:[^"\\]|\\.)*)"', query):
        if op != "!=":
            out[label] = (op, unquote(value))
    return out


def window_seconds(query, default):
    m = re.search(r"\[(\d+)([smhd])\]", query)
    return int(m.group(1)) * {"s": 1, "m": 60, "h": 3600, "d": 86400}[m.group(2)] if m else default


def evaluate(cluster, query):
    """Returns [(labels, fn(t) -> value)] for a query Skiff sends."""
    series = instant(cluster, query)
    if "rate(" not in query:
        return series
    # rate() averages over its window: so do these, or a week at two-hour
    # steps would be sampled noise.
    window = window_seconds(query, 120)

    def averaged(fn):
        return lambda t: sum(fn(t - window * i / 8.0) for i in range(8)) / 8.0

    return [(labels, averaged(fn)) for labels, fn in series]


def instant(cluster, query):
    if re.fullmatch(r"\s*vector\(\s*1\s*\)\s*", query):
        return [({}, lambda t: 1.0)]
    metric = next((m for m in METRICS if m in query), None)
    if metric is None:
        return []
    sel = matchers(query)
    by = re.search(r"\bby\s*\(\s*([a-zA-Z_]+)\s*\)", query)
    by = by.group(1) if by else None
    namespace = sel.get("namespace", ("=", ""))[1]

    # Nodes: the root cgroup, by `node` label or by `instance`.
    node = None
    if "node" in sel:
        node = sel["node"][1]
    elif "instance" in sel:
        # Skiff sends  name(:[0-9]+)?  with the name regex-escaped.
        node = unquote(sel["instance"][1].split("(:")[0])
    if metric == "kube_node_status_allocatable":
        alloc = cluster.node(node)
        value = alloc["cpu"] if sel.get("resource", ("", ""))[1] == "cpu" else alloc["memory"]
        return [({}, lambda t: value)]
    if node is not None and "pod" not in sel:
        alloc = cluster.node(node)
        if metric == "container_cpu_usage_seconds_total":
            return [({}, lambda t: node_cpu(node, alloc["cpu"], t))]
        if metric == "container_memory_working_set_bytes":
            return [({}, lambda t: node_memory(node, alloc["memory"], t))]
        if metric.startswith("container_network_"):
            direction = "rx" if "receive" in metric else "tx"
            return [({}, lambda t: 40 * network_bytes(node, direction, t))]
        return []

    # Pods: one name, or the alternation Skiff builds for a workload.
    if "pod" not in sel:
        return []
    op, value = sel["pod"]
    names = [unquote(n) for n in value.split("|")] if op == "=~" else [value]
    pods = [(name, cluster.pod(namespace, name)) for name in names if name]

    if metric in ("kube_pod_container_resource_requests", "kube_pod_container_resource_limits"):
        field = ("cpu" if sel.get("resource", ("", ""))[1] == "cpu" else "memory") + \
            ("_request" if metric.endswith("requests") else "_limit")
        per_pod = [sum(c[field] for c in info["containers"] if c[field]) for _, info in pods]
        per_pod = [v for v in per_pod if v]
        # A workload chart asks for max(sum by(pod)(...)): what one pod gets.
        total = max(per_pod, default=0) if by == "pod" else sum(per_pod)
        return [({}, lambda t: total)] if per_pod else []

    if metric in ("container_cpu_usage_seconds_total", "container_memory_working_set_bytes"):
        fn = cpu_cores if metric.startswith("container_cpu") else memory_bytes
        series = []
        for name, info in pods:
            if by == "container":
                for c in info["containers"]:
                    series.append(({"container": c["name"]},
                                   lambda t, name=name, c=c, info=info: fn(name, c, info["crashloop"], t)))
            else:
                series.append(({"pod": name} if by == "pod" else {},
                               lambda t, name=name, info=info: sum(
                                   fn(name, c, info["crashloop"], t) for c in info["containers"])))
        if by is None and series:
            fns = [f for _, f in series]
            return [({}, lambda t: sum(f(t) for f in fns))]
        return series

    if metric.startswith("container_network_"):
        direction = "rx" if "receive" in metric else "tx"
        return [({}, lambda t: sum(network_bytes(name, direction, t) for name, _ in pods))]

    if metric == "kube_pod_container_status_restarts_total":
        window = window_seconds(query, 300)
        return [({}, lambda t: sum(restarts(name, info["crashloop"], t - window, t) for name, info in pods))]
    return []


def number(v):
    return repr(round(v, 6)) if abs(v) < 1e15 else repr(v)


def alerts(cluster):
    now = time.time()

    def at(minutes_ago):
        return time.strftime("%Y-%m-%dT%H:%M:%S.000000000Z", time.gmtime(now - 60 * minutes_ago))

    def alert(name, severity, minutes, summary, description, state="firing", **labels):
        return {
            "labels": {"alertname": name, "severity": severity, **labels},
            "annotations": {"summary": summary, "description": description},
            "state": state, "activeAt": at(minutes), "value": "1e+00",
        }

    out = [
        alert("Watchdog", "none", 5000, "An alert that should always be firing.", "This is the dead man's switch."),
        alert("KubeNodeNotReady", "critical", 47, "Node is not ready.",
              "demo-worker-5 has been unready for more than 15 minutes.", node="demo-worker-5"),
        alert("KubeDeploymentReplicasMismatch", "warning", 83, "Deployment has not matched the expected number of replicas.",
              "Deployment payments/ledger has not matched the expected number of replicas for longer than 15 minutes.",
              namespace="payments", deployment="ledger", pod="kube-state-metrics-54b6975f9f-wvpzl",
              job="kube-state-metrics"),
        alert("NodeFilesystemAlmostOutOfSpace", "warning", 212, "Filesystem has less than 5% space left.",
              "Filesystem on /dev/nvme0n1p1 at demo-worker-2:9100 has only 4.31% available space left.",
              instance="demo-worker-2:9100", job="node-exporter", device="/dev/nvme0n1p1", mountpoint="/"),
        alert("TargetDown", "warning", 26, "One or more targets are unreachable.",
              "100% of the kube-scheduler/kube-scheduler targets in kube-system namespace are down.",
              namespace="kube-system", job="kube-scheduler", service="kube-scheduler"),
        alert("KubeMemoryOvercommit", "warning", 3, "Cluster has overcommitted memory resource requests.",
              "Still pending: must not be shown.", state="pending"),
    ]
    for namespace, prefix, container, minutes in (("data", "etl-worker-", "etl-worker", 61),
                                                  ("payments", "fraud-detector-", "fraud-detector", 38)):
        pod = cluster.first_pod(namespace, prefix)
        if pod:
            out.append(alert(
                "KubePodCrashLooping", "warning", minutes, "Pod is crash looping.",
                f'Pod {namespace}/{pod} ({container}) is in waiting state (reason: "CrashLoopBackOff").',
                namespace=namespace, pod=pod, container=container, job="kube-state-metrics"))
    pod = cluster.first_pod("shop", "api-")
    if pod:
        out.append(alert(
            "CPUThrottlingHigh", "info", 17, "Processes experience elevated CPU throttling.",
            f"31.4% throttling of CPU in namespace shop for container api in pod {pod}.",
            namespace="shop", pod=pod, container="api"))
    return out


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    cluster = None

    def log_message(self, *args):
        pass

    def reply(self, status, body, content_type="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def bad(self, message):
        self.reply(400, {"status": "error", "errorType": "bad_data", "error": message})

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        params = {k: v[-1] for k, v in urllib.parse.parse_qs(url.query).items()}
        path = url.path
        if path.startswith("/deny/"):
            return self.reply(403, b"Forbidden: this Prometheus only answers its own service account\n", "text/plain")
        if path.startswith("/broken/"):
            return self.reply(500, b"<html><body><h1>500 Internal Server Error</h1></body></html>", "text/html")
        try:
            if path == "/api/v1/alerts":
                return self.reply(200, {"status": "success", "data": {"alerts": alerts(self.cluster)}})
            if path == "/api/v1/query":
                t = float(params.get("time", time.time()))
                result = [{"metric": labels, "value": [t, number(fn(t))]}
                          for labels, fn in evaluate(self.cluster, params.get("query", ""))]
                return self.reply(200, {"status": "success", "data": {"resultType": "vector", "result": result}})
            if path == "/api/v1/query_range":
                start, end, step = float(params["start"]), float(params["end"]), float(params["step"])
                if step <= 0 or end < start:
                    return self.bad("invalid parameter: start, end or step")
                if (end - start) / step > 11000:
                    return self.bad("exceeded maximum resolution of 11,000 points per timeseries")
                times = [start + i * step for i in range(int((end - start) / step) + 1)]
                result = [{"metric": labels, "values": [[t, number(fn(t))] for t in times]}
                          for labels, fn in evaluate(self.cluster, params.get("query", ""))]
                return self.reply(200, {"status": "success", "data": {"resultType": "matrix", "result": result}})
        except (KeyError, ValueError) as e:
            return self.bad(f"invalid parameter: {e}")
        if path in ("/-/healthy", "/-/ready"):
            return self.reply(200, b"Fake Prometheus is Healthy.\n", "text/plain")
        self.reply(404, b"404 page not found\n", "text/plain")


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main(argv):
    if "--print-address" in argv:
        print(host_address())
        return 0
    args = [a for a in argv if not a.startswith("--")]
    options = {argv[i]: argv[i + 1] for i in range(len(argv) - 1) if argv[i].startswith("--")}
    args = [a for a in args if a not in options.values()]
    port = int(args[0]) if args else 18091
    Handler.cluster = Cluster(options.get("--kubeconfig"), options.get("--context"))
    # The API server reaches this through the host's non-loopback address
    # (see host_address); listen there only, not on every interface.
    Server((options.get("--bind") or host_address() or "0.0.0.0", port), Handler).serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
