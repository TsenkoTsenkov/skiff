#!/usr/bin/env python3
"""Generates 10-workloads.yaml (run: python3 -I gen_workloads.py > 10-workloads.yaml)."""
docs = []
TOL_ALL = "[{operator: Exists}]"

def labels(d): return "{" + ", ".join(f'{k}: "{v}"' for k, v in d.items()) + "}"

def pod_spec(name, image, port=None, cpu="50m", mem="64Mi", env=None, sa=None, tol=None, nsel=None,
             extra_res="", volumes="", vmounts="", init=False, cmd=None, limits=True, prio=None):
    s = []
    if sa: s.append(f"      serviceAccountName: {sa}")
    if prio: s.append(f"      priorityClassName: {prio}")
    if nsel: s.append(f"      nodeSelector: {nsel}")
    if tol: s.append(f"      tolerations: {tol}")
    if init:
        s.append(f"      initContainers:\n      - {{name: init-wait, image: busybox:1.36, command: [sh, -c, 'echo waiting for deps; sleep 1']}}")
    c = f"      - name: {name}\n        image: {image}"
    if cmd: c += f"\n        command: {cmd}"
    if port: c += f"\n        ports: [{{name: http, containerPort: {port}}}]"
    if env: c += "\n        env:\n" + "\n".join(f"        - {{name: {k}, value: \"{v}\"}}" for k, v in env.items())
    def _mi(m): return int(m[:-2]) * (1024 if m.endswith("Gi") else 1)
    def _m(c): return int(c[:-1]) if c.endswith("m") else int(c) * 1000
    lim = f", limits: {{cpu: {_m(cpu)*4}m, memory: {_mi(mem)*2}Mi{extra_res}}}" if limits else ""
    c += f"\n        resources: {{requests: {{cpu: {cpu}, memory: {mem}{extra_res}}}{lim}}}"
    if vmounts: c += f"\n        volumeMounts:\n{vmounts}"
    s.append("      containers:\n" + c)
    if volumes: s.append(f"      volumes:\n{volumes}")
    return "\n".join(s)

def deploy(ns, name, replicas, image, app=None, port=8080, state=None, anns=None, extra_labels=None, rev_label=None, **kw):
    app = app or name
    lab = {"app": app, "app.kubernetes.io/name": app, "app.kubernetes.io/part-of": ns}
    if extra_labels: lab.update(extra_labels)
    plab = dict(lab)
    if state: plab["sim.skiff.dev/state"] = state
    pann = ""
    if anns: pann = "\n      annotations: " + labels(anns)
    docs.append(f"""apiVersion: apps/v1
kind: Deployment
metadata: {{name: {name}, namespace: {ns}, labels: {labels(lab)}}}
spec:
  replicas: {replicas}
  revisionHistoryLimit: 5
  selector: {{matchLabels: {{app: {app}}}}}
  strategy: {{type: RollingUpdate, rollingUpdate: {{maxSurge: 1, maxUnavailable: 1}}}}
  template:
    metadata:
      labels: {labels(plab)}{pann}
    spec:
{pod_spec(name, image, port, **kw)}""")

def sts(ns, name, replicas, image, port, pvc=None, state=None, anns=None, **kw):
    lab = {"app": name, "app.kubernetes.io/name": name, "app.kubernetes.io/part-of": ns}
    vct = ""
    vm = ""
    if pvc:
        sc, size = pvc
        vct = f"""  volumeClaimTemplates:
  - metadata: {{name: data}}
    spec:
      accessModes: [ReadWriteOnce]
      storageClassName: {sc}
      resources: {{requests: {{storage: {size}}}}}"""
        vm = "        - {name: data, mountPath: /data}"
    pann = ("\n      annotations: " + labels(anns)) if anns else ""
    docs.append(f"""apiVersion: apps/v1
kind: StatefulSet
metadata: {{name: {name}, namespace: {ns}, labels: {labels(lab)}}}
spec:
  replicas: {replicas}
  serviceName: {name}-headless
  podManagementPolicy: OrderedReady
  selector: {{matchLabels: {{app: {name}}}}}
  template:
    metadata:
      labels: {labels(lab)}{pann}
    spec:
{pod_spec(name, image, port, vmounts=vm, **kw)}
{vct}""")
    svc(ns, f"{name}-headless", name, port, headless=True)

def ds(ns, name, image, port=None, tol=TOL_ALL, anns=None, **kw):
    lab = {"app": name, "app.kubernetes.io/name": name}
    pann = ("\n      annotations: " + labels(anns)) if anns else ""
    docs.append(f"""apiVersion: apps/v1
kind: DaemonSet
metadata: {{name: {name}, namespace: {ns}, labels: {labels(lab)}}}
spec:
  selector: {{matchLabels: {{app: {name}}}}}
  updateStrategy: {{type: RollingUpdate, rollingUpdate: {{maxUnavailable: 1}}}}
  template:
    metadata:
      labels: {labels(lab)}{pann}
    spec:
      hostNetwork: false
{pod_spec(name, image, port, tol=tol, **kw)}""")

def svc(ns, name, app, port, tport=None, type_="ClusterIP", headless=False, nodeport=None):
    extra = "\n  clusterIP: None" if headless else ""
    np = f", nodePort: {nodeport}" if nodeport else ""
    docs.append(f"""apiVersion: v1
kind: Service
metadata: {{name: {name}, namespace: {ns}, labels: {{app: {app}}}}}
spec:
  type: {type_}{extra}
  selector: {{app: {app}}}
  ports:
  - {{name: http, port: {port}, targetPort: {tport or port}, protocol: TCP{np}}}""")

def hpa(ns, name, target, mn, mx, cpu=70, mem=None):
    metrics = f"  - {{type: Resource, resource: {{name: cpu, target: {{type: Utilization, averageUtilization: {cpu}}}}}}}"
    if mem: metrics += f"\n  - {{type: Resource, resource: {{name: memory, target: {{type: Utilization, averageUtilization: {mem}}}}}}}"
    docs.append(f"""apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata: {{name: {name}, namespace: {ns}}}
spec:
  scaleTargetRef: {{apiVersion: apps/v1, kind: Deployment, name: {target}}}
  minReplicas: {mn}
  maxReplicas: {mx}
  metrics:
{metrics}""")

def cm(ns, name, data):
    body = "\n".join(f"  {k}: |\n" + "\n".join("    " + l for l in v.split("\n")) for k, v in data.items())
    docs.append(f"apiVersion: v1\nkind: ConfigMap\nmetadata: {{name: {name}, namespace: {ns}}}\ndata:\n{body}")

def secret(ns, name, string_data, type_="Opaque"):
    body = "\n".join(f"  {k}: '{v}'" for k, v in string_data.items())
    docs.append(f"apiVersion: v1\nkind: Secret\nmetadata: {{name: {name}, namespace: {ns}}}\ntype: {type_}\nstringData:\n{body}")

def ingress(ns, name, rules, tls=None, cls="nginx", anns=None):
    r = "\n".join(f"""  - host: {h}
    http:
      paths:
      - {{path: {p}, pathType: Prefix, backend: {{service: {{name: {s}, port: {{number: {pt}}}}}}}}}""" for h, p, s, pt in rules)
    t = f"\n  tls:\n  - {{hosts: [{tls[0]}], secretName: {tls[1]}}}" if tls else ""
    a = f"\n  annotations: {labels(anns)}" if anns else ""
    docs.append(f"""apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: {name}
  namespace: {ns}{a}
spec:
  ingressClassName: {cls}{t}
  rules:
{r}""")

def netpol(ns, name, spec):
    docs.append(f"apiVersion: networking.k8s.io/v1\nkind: NetworkPolicy\nmetadata: {{name: {name}, namespace: {ns}}}\nspec:\n{spec}")

def pdb(ns, name, app, minav):
    docs.append(f"apiVersion: policy/v1\nkind: PodDisruptionBudget\nmetadata: {{name: {name}, namespace: {ns}}}\nspec:\n  minAvailable: {minav}\n  selector: {{matchLabels: {{app: {app}}}}}")

def job(ns, name, image, state=None, backoff=3, completions=1, parallelism=1, keep=False, anns=None):
    plab = {"app": name, "job-name": name}
    if state: plab["sim.skiff.dev/state"] = state
    if keep: plab["sim.skiff.dev/no-complete"] = "true"
    docs.append(f"""apiVersion: batch/v1
kind: Job
metadata: {{name: {name}, namespace: {ns}, labels: {{app: {name}}}}}
spec:
  backoffLimit: {backoff}
  completions: {completions}
  parallelism: {parallelism}
  template:
    metadata:
      labels: {labels(plab)}
    spec:
      restartPolicy: Never
      serviceAccountName: batch-runner
      containers:
      - {{name: main, image: {image}, command: [sh, -c, "run-task"], resources: {{requests: {{cpu: 100m, memory: 128Mi}}}}}}""")

def cronjob(ns, name, schedule, image, suspend=False, hist=3):
    docs.append(f"""apiVersion: batch/v1
kind: CronJob
metadata: {{name: {name}, namespace: {ns}, labels: {{app: {name}}}}}
spec:
  schedule: "{schedule}"
  suspend: {str(suspend).lower()}
  concurrencyPolicy: Forbid
  successfulJobsHistoryLimit: {hist}
  failedJobsHistoryLimit: 1
  jobTemplate:
    spec:
      template:
        metadata: {{labels: {{app: {name}}}}}
        spec:
          restartPolicy: OnFailure
          serviceAccountName: batch-runner
          containers:
          - {{name: main, image: {image}, command: [sh, -c, "run-task"], resources: {{requests: {{cpu: 100m, memory: 128Mi}}}}}}""")

U = lambda cpu, mem: {"kwok.x-k8s.io/usage-cpu": cpu, "kwok.x-k8s.io/usage-memory": mem}
PROM_ANN = {"prometheus.io/scrape": "true", "prometheus.io/port": "9090"}

# ---------------- kube-system ----------------
deploy("kube-system", "coredns", 2, "registry.k8s.io/coredns/coredns:v1.11.3", app="coredns", port=53,
       anns=U("8m", "28Mi"), tol="[{key: node-role.kubernetes.io/control-plane, operator: Exists, effect: NoSchedule}]", extra_labels={"k8s-app": "kube-dns"})
ds("kube-system", "kube-proxy", "registry.k8s.io/kube-proxy:v1.37.2", anns=U("4m", "22Mi"))

# ---------------- shop ----------------
cm("shop", "web-config", {"nginx.conf": "worker_processes auto;\nevents { worker_connections 1024; }\nhttp {\n  upstream api { server api:8080; }\n  server {\n    listen 80;\n    location /api/ { proxy_pass http://api; }\n    location / { root /usr/share/nginx/html; }\n  }\n}", "LOG_LEVEL": "info"})
cm("shop", "api-config", {"application.yaml": "server:\n  port: 8080\ndatabase:\n  host: postgres-0.postgres-headless\n  pool: 20\ncache:\n  host: redis-0.redis-headless\n  ttl: 300s"})
cm("shop", "feature-flags", {"flags.json": '{\n  "newCheckout": true,\n  "recommendations": true,\n  "darkMode": false,\n  "betaSearch": {"enabled": true, "percent": 25}\n}'})
secret("shop", "db-credentials", {"username": "shop_app", "password": "s3cr3t-not-real", "host": "postgres-0.postgres-headless"})
secret("shop", "api-token", {"token": "tok_live_0123456789abcdef"})
secret("shop", "registry-cred", {".dockerconfigjson": '{"auths":{"registry.example.com":{"username":"ci","password":"x","auth":"Y2k6eA=="}}}'}, "kubernetes.io/dockerconfigjson")
deploy("shop", "web", 3, "nginx:1.25-alpine", port=80, sa="web", cpu="100m", mem="128Mi", anns=U("70m", "90Mi"),
       env={"API_URL": "http://api:8080"}, init=True,
       volumes="      - {name: cfg, configMap: {name: web-config}}", vmounts="        - {name: cfg, mountPath: /etc/nginx/conf.d, readOnly: true}",
       extra_labels={"tier": "frontend"}, prio="business-critical")
deploy("shop", "api", 4, "registry.example.com/shop/api:2.14.1", port=8080, sa="api", cpu="200m", mem="256Mi", anns=U("150m", "180Mi"),
       env={"DB_HOST": "postgres-0.postgres-headless"}, extra_labels={"tier": "backend"})
deploy("shop", "cart", 2, "registry.example.com/shop/cart:1.8.0", port=8081, cpu="100m", mem="128Mi", anns=U("20m", "70Mi"), extra_labels={"tier": "backend"})
deploy("shop", "recommender", 4, "registry.example.com/shop/recommender-gpu:0.9.3", port=9000, cpu="500m", mem="2Gi", extra_res=", nvidia.com/gpu: 1",
       nsel="{accelerator: nvidia-a100}", tol="[{key: nvidia.com/gpu, operator: Exists, effect: NoSchedule}]",
       anns=U("350m", "1500Mi"), extra_labels={"tier": "ml"})
sts("shop", "redis", 3, "redis:7.2-alpine", 6379, pvc=("standard", "2Gi"), anns=U("10m", "30Mi"))
sts("shop", "postgres", 2, "postgres:16.2", 5432, pvc=("fast-ssd", "50Gi"), cpu="250m", mem="512Mi", anns=U("120m", "400Mi"))
svc("shop", "web", "web", 80)
svc("shop", "api", "api", 8080)
svc("shop", "cart", "cart", 8081)
svc("shop", "web-lb", "web", 443, 80, type_="LoadBalancer")
svc("shop", "api-nodeport", "api", 8080, type_="NodePort", nodeport=30080)
svc("shop", "recommender", "recommender", 9000)
docs.append("apiVersion: v1\nkind: Service\nmetadata: {name: external-payments, namespace: shop}\nspec: {type: ExternalName, externalName: api.payments-provider.example.com}")
hpa("shop", "web", "web", 3, 10, 70)
hpa("shop", "api", "api", 4, 12, 60, mem=80)
pdb("shop", "web-pdb", "web", 2)
ingress("shop", "shop", [("shop.example.com", "/", "web", 80), ("api.shop.example.com", "/", "api", 8080)], tls=("shop.example.com", "shop-tls-secret"),
        anns={"nginx.ingress.kubernetes.io/proxy-body-size": "10m", "cert-manager.io/cluster-issuer": "letsencrypt-prod"})
netpol("shop", "default-deny-ingress", "  podSelector: {}\n  policyTypes: [Ingress]")
netpol("shop", "allow-web-to-api", "  podSelector: {matchLabels: {app: api}}\n  policyTypes: [Ingress]\n  ingress:\n  - from: [{podSelector: {matchLabels: {app: web}}}]\n    ports: [{protocol: TCP, port: 8080}]")
netpol("shop", "allow-api-to-db", "  podSelector: {matchLabels: {app: postgres}}\n  policyTypes: [Ingress, Egress]\n  ingress:\n  - from: [{podSelector: {matchLabels: {app: api}}}]\n    ports: [{protocol: TCP, port: 5432}]\n  egress:\n  - to: [{namespaceSelector: {matchLabels: {team: data-platform}}}]")

# ---------------- payments ----------------
deploy("payments", "gateway", 3, "registry.example.com/payments/gateway:5.2.0", port=8443, cpu="250m", mem="256Mi", anns=U("180m", "200Mi"), prio="business-critical")
deploy("payments", "notifier", 2, "registry.example.com/payments/notifier:1.1.4", port=8080, anns=U("12m", "50Mi"))
deploy("payments", "fraud-detector", 2, "registry.example.com/payments/fraud:3.0.0-rc1", port=8080, state="crashloop", anns={"sim.skiff.dev/restarts": "14"})
# ledger is created separately (rollout scenario) in demo.sh
svc("payments", "gateway", "gateway", 8443)
svc("payments", "ledger", "ledger", 8080)
svc("payments", "fraud-detector", "fraud-detector", 8080)
hpa("payments", "gateway", "gateway", 3, 8, 65)
secret("payments", "stripe-keys", {"secret_key": "sk_test_not_real", "webhook_secret": "whsec_not_real"})
secret("payments", "gateway-mtls-secret", {"tls.crt": "-----BEGIN CERTIFICATE----- MIIBplaceholder -----END CERTIFICATE-----", "tls.key": "-----BEGIN PRIVATE KEY----- MIIBplaceholder -----END PRIVATE KEY-----"})
cm("payments", "gateway-config", {"routes.yaml": "routes:\n  - path: /charge\n    upstream: ledger:8080\n  - path: /refund\n    upstream: ledger:8080"})
netpol("payments", "pci-isolation", "  podSelector: {}\n  policyTypes: [Ingress, Egress]\n  ingress:\n  - from: [{namespaceSelector: {matchLabels: {team: storefront}}}]\n  egress:\n  - to: [{namespaceSelector: {}}]\n    ports: [{protocol: TCP, port: 443}]")

# ---------------- monitoring ----------------
cm("monitoring", "prometheus-config", {"prometheus.yml": "global:\n  scrape_interval: 15s\nscrape_configs:\n  - job_name: kubernetes-pods\n    kubernetes_sd_configs: [{role: pod}]\n  - job_name: nodes\n    kubernetes_sd_configs: [{role: node}]"})
cm("monitoring", "grafana-dashboards", {"cluster-overview.json": '{"title":"Cluster Overview","panels":[]}', "node-detail.json": '{"title":"Node Detail","panels":[]}'})
sts("monitoring", "prometheus", 1, "quay.io/prometheus/prometheus:v2.53.0", 9090, pvc=("standard", "100Gi"), cpu="500m", mem="1Gi", sa="prometheus", anns=U("300m", "900Mi"))
sts("monitoring", "alertmanager", 2, "quay.io/prometheus/alertmanager:v0.27.0", 9093, anns=U("5m", "25Mi"))
deploy("monitoring", "grafana", 2, "grafana/grafana:11.1.0", port=3000, cpu="100m", mem="256Mi", anns=U("40m", "180Mi"))
deploy("monitoring", "kube-state-metrics", 1, "registry.k8s.io/kube-state-metrics/kube-state-metrics:v2.13.0", port=8080, anns=U("15m", "60Mi"))
ds("monitoring", "node-exporter", "quay.io/prometheus/node-exporter:v1.8.1", port=9100, anns=U("6m", "20Mi"))
svc("monitoring", "prometheus", "prometheus", 9090)
svc("monitoring", "alertmanager", "alertmanager", 9093)
svc("monitoring", "grafana", "grafana", 3000)
svc("monitoring", "kube-state-metrics", "kube-state-metrics", 8080)
ingress("monitoring", "grafana", [("grafana.example.com", "/", "grafana", 3000)], tls=("grafana.example.com", "grafana-tls-secret"))
secret("monitoring", "grafana-admin", {"admin-user": "admin", "admin-password": "not-a-real-password"})

# ---------------- data ----------------
sts("data", "kafka", 3, "confluentinc/cp-kafka:7.6.1", 9092, pvc=("standard", "200Gi"), cpu="500m", mem="2Gi", anns=U("250m", "1600Mi"))
deploy("data", "schema-registry", 2, "confluentinc/cp-schema-registry:7.6.1", port=8081, cpu="100m", mem="512Mi", anns=U("30m", "350Mi"))
deploy("data", "analytics-spark", 2, "registry.example.com/data/spark-thrift:3.5.1", port=10000, cpu="2", mem="16Gi", limits=False,
       nsel="{workload: memory}", tol="[{key: dedicated, operator: Equal, value: analytics, effect: NoSchedule}]", anns=U("1500m", "12Gi"))
deploy("data", "etl-worker", 2, "registry.example.com/data/etl:4.2.0", port=8080, state="oomkilled", anns={"sim.skiff.dev/restarts": "31"})
svc("data", "kafka", "kafka", 9092)
svc("data", "schema-registry", "schema-registry", 8081)
cm("data", "kafka-topics", {"topics.txt": "orders\npayments\nclickstream\ninventory-updates"})

# ---------------- batch ----------------
cronjob("batch", "nightly-backup", "*/2 * * * *", "registry.example.com/ops/backup:1.4.0")
cronjob("batch", "hourly-report", "*/5 * * * *", "registry.example.com/data/report:2.0.1")
cronjob("batch", "weekly-cleanup", "0 3 * * 0", "busybox:1.36", suspend=True)
job("batch", "db-migrate-20261101", "registry.example.com/ops/migrate:7.3.0")
job("batch", "index-rebuild", "registry.example.com/search/indexer:2.2.0", completions=6, parallelism=3)
job("batch", "data-import-running", "registry.example.com/data/import:1.0.0", keep=True)

# ---------------- platform ----------------
deploy("platform", "ingress-nginx-controller", 2, "registry.k8s.io/ingress-nginx/controller:v1.11.1", port=80, cpu="100m", mem="128Mi", anns=U("45m", "150Mi"), extra_labels={"app.kubernetes.io/component": "controller"})
deploy("platform", "cert-manager", 1, "quay.io/jetstack/cert-manager-controller:v1.15.0", port=9402, anns=U("8m", "45Mi"))
deploy("platform", "cert-manager-webhook", 2, "quay.io/jetstack/cert-manager-webhook:v1.15.0", port=10250, anns=U("4m", "20Mi"))
ds("platform", "fluent-bit", "fluent/fluent-bit:3.1.0", anns=U("25m", "60Mi"))
svc("platform", "ingress-nginx-controller", "ingress-nginx-controller", 80)
secret("platform", "registry-cred", {".dockerconfigjson": '{"auths":{"registry.example.com":{"username":"ci","password":"x","auth":"Y2k6eA=="}}}'}, "kubernetes.io/dockerconfigjson")
cm("platform", "cluster-info", {"region": "us-east-1", "cluster": "demo", "environment": "production"})

# ---------------- incidents (deliberately unhealthy) ----------------
deploy("incidents", "ghost-image", 2, "registry.example.com/missing/app:v9.9.9", state="imagepull")
deploy("incidents", "registry-denied", 1, "private.registry.example.com/secret/app:1.0", state="errimagepull")
deploy("incidents", "crashy", 3, "registry.example.com/incidents/crashy:1.0", state="crashloop", anns={"sim.skiff.dev/restarts": "23"})
deploy("incidents", "oom-service", 1, "registry.example.com/incidents/leaky:2.1", state="oomkilled")
deploy("incidents", "bad-config", 2, "registry.example.com/incidents/needs-secret:1.3", state="configerror")
deploy("incidents", "unready-svc", 3, "registry.example.com/incidents/flaky:0.4", port=8080, state="notready")
deploy("incidents", "pending-gpu", 2, "registry.example.com/ml/trainer:1.0", cpu="1", mem="4Gi", extra_res=", nvidia.com/gpu: 8",
       tol="[{key: nvidia.com/gpu, operator: Exists, effect: NoSchedule}]")
deploy("incidents", "pending-affinity", 1, "nginx:1.25", cpu="100m", mem="64Mi", nsel="{disktype: nvme-raid}")
docs.append("""apiVersion: v1
kind: PersistentVolumeClaim
metadata: {name: data-claim, namespace: incidents}
spec:
  accessModes: [ReadWriteOnce]
  storageClassName: nonexistent-class
  resources: {requests: {storage: 10Gi}}""")
docs.append("""apiVersion: apps/v1
kind: Deployment
metadata: {name: pvc-stuck, namespace: incidents, labels: {app: pvc-stuck}}
spec:
  replicas: 1
  selector: {matchLabels: {app: pvc-stuck}}
  template:
    metadata: {labels: {app: pvc-stuck}}
    spec:
      containers:
      - {name: app, image: nginx:1.25, volumeMounts: [{name: data, mountPath: /data}]}
      volumes:
      - {name: data, persistentVolumeClaim: {claimName: data-claim}}""")
job("incidents", "report-always-fails", "registry.example.com/data/report:2.0.1", state="error", backoff=3)
job("incidents", "ok-job", "busybox:1.36")
docs.append("""apiVersion: v1
kind: Pod
metadata: {name: debug-shell, namespace: incidents, labels: {app: debug-shell, purpose: debug}}
spec:
  containers:
  - {name: shell, image: busybox:1.36, command: [sleep, "3600"], resources: {requests: {cpu: 10m, memory: 16Mi}}}""")
docs.append("""apiVersion: v1
kind: Pod
metadata: {name: multi-container, namespace: incidents, labels: {app: multi-container}}
spec:
  initContainers:
  - {name: init-db, image: busybox:1.36, command: [sh, -c, "echo init"]}
  containers:
  - {name: app, image: nginx:1.25, resources: {requests: {cpu: 50m, memory: 64Mi}}}
  - {name: sidecar-proxy, image: envoyproxy/envoy:v1.30, resources: {requests: {cpu: 50m, memory: 64Mi}}}
  - {name: log-agent, image: fluent/fluent-bit:3.1.0, resources: {requests: {cpu: 10m, memory: 32Mi}}}""")

# ---------------- team-a / team-b ----------------
deploy("team-a", "notebook", 1, "jupyter/base-notebook:2024-06", port=8888, cpu="500m", mem="1Gi", anns=U("200m", "700Mi"))
deploy("team-a", "api-dev", 2, "registry.example.com/team-a/api:dev", port=8080, anns=U("25m", "80Mi"))
deploy("team-a", "redis-dev", 1, "redis:7.2-alpine", port=6379)
svc("team-a", "api-dev", "api-dev", 8080)
cm("team-a", "settings", {"DEBUG": "true", "REGION": "eu-west-1"})
deploy("team-b", "demo-app", 2, "nginx:1.25-alpine", port=80)
svc("team-b", "demo-app-lb", "demo-app", 80, type_="LoadBalancer")
cm("team-b", "settings", {"DEBUG": "false"})

print("\n---\n".join(docs))
