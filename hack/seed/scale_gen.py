#!/usr/bin/env python3
"""Generate the 'scale' cluster seed into a directory: python3 -I scale_gen.py OUTDIR
~100 nodes, 20 namespaces, 320 Deployments (~5.5k pods), 3000 each ConfigMaps/Secrets/Services."""
import os, sys
out = sys.argv[1]
os.makedirs(out, exist_ok=True)
NODES, NS, DEPLOY_PER_NS, OBJ_PER_NS = 100, 20, 16, 150

def w(name, docs):
    with open(os.path.join(out, name), "w") as f:
        f.write("\n---\n".join(docs) + "\n")

# --- nodes ---
zones = ["us-east-1a", "us-east-1b", "us-east-1c", "us-west-2a", "us-west-2b"]
itypes = ["m5.2xlarge", "m5.4xlarge", "c5.4xlarge", "r5.2xlarge"]
nodes = []
for i in range(1, NODES + 1):
    n = f"scale-node-{i:03d}"
    z = zones[i % len(zones)]
    pool = "general" if i <= 70 else ("compute" if i <= 90 else ("memory" if i <= 97 else "spot"))
    taint = ""
    if pool == "memory": taint = "\n  taints: [{key: dedicated, value: memory, effect: NoSchedule}]"
    if pool == "spot": taint = "\n  taints: [{key: spot, value: \"true\", effect: PreferNoSchedule}]"
    cpu, mem = {"general": ("16", "64Gi"), "compute": ("32", "64Gi"), "memory": ("32", "256Gi"), "spot": ("8", "32Gi")}[pool]
    unsched = "\n  unschedulable: true" if i in (5, 6) else ""
    lab = f"""{{type: kwok, "kubernetes.io/hostname": {n}, "kubernetes.io/os": linux, "kubernetes.io/arch": {"arm64" if i % 7 == 0 else "amd64"},
    "node-role.kubernetes.io/worker": "", "topology.kubernetes.io/zone": {z}, "topology.kubernetes.io/region": {z[:-1]},
    "node.kubernetes.io/instance-type": {itypes[i % 4]}, pool: {pool}}}"""
    nodes.append(f"""apiVersion: v1
kind: Node
metadata:
  name: {n}
  annotations: {{kwok.x-k8s.io/node: fake, "metrics.k8s.io/resource-metrics-path": "/metrics/nodes/{n}/metrics/resource", "node.alpha.kubernetes.io/ttl": "0"}}
  labels: {lab}
spec:{taint}{unsched}
  providerID: kwok://{n}
status:
  allocatable: {{cpu: "{cpu}", memory: {mem}, pods: "250", ephemeral-storage: 500Gi}}
  capacity: {{cpu: "{cpu}", memory: {mem}, pods: "250", ephemeral-storage: 500Gi}}
  nodeInfo: {{kubeletVersion: v1.37.2, osImage: Ubuntu 24.04 LTS, kernelVersion: 6.8.0-aws, containerRuntimeVersion: containerd://1.7.18, architecture: amd64, operatingSystem: linux}}""")
w("00-nodes.yaml", nodes)

# --- namespaces ---
nss = [f"ns-{i:02d}" for i in range(1, NS + 1)]
w("01-namespaces.yaml", [f"apiVersion: v1\nkind: Namespace\nmetadata: {{name: {n}, labels: {{tenant: t{(i % 5)}, tier: {['gold','silver','bronze'][i % 3]}}}}}" for i, n in enumerate(nss)])

# --- per-namespace objects ---
total_pods = 0
for ni, ns in enumerate(nss):
    docs = []
    for d in range(DEPLOY_PER_NS):
        idx = ni * DEPLOY_PER_NS + d
        reps = 10 + (idx % 15)            # 10..24, avg 17
        total_pods += reps
        name = f"app-{d:02d}"
        state = ""
        if idx % 40 == 7: state = ', "sim.skiff.dev/state": crashloop'
        elif idx % 55 == 11: state = ', "sim.skiff.dev/state": imagepull'
        docs.append(f"""apiVersion: apps/v1
kind: Deployment
metadata: {{name: {name}, namespace: {ns}, labels: {{app: {name}, tier: {['web','api','worker','cache'][d % 4]}}}}}
spec:
  replicas: {reps}
  selector: {{matchLabels: {{app: {name}}}}}
  template:
    metadata: {{labels: {{app: {name}, tier: {['web','api','worker','cache'][d % 4]}{state}}}}}
    spec:
      containers:
      - name: main
        image: registry.example.com/{ns}/{name}:1.{idx % 20}.0
        resources: {{requests: {{cpu: 20m, memory: 32Mi}}}}
        envFrom: [{{configMapRef: {{name: cfg-000}}}}]""")
    for k in range(OBJ_PER_NS):
        docs.append(f"apiVersion: v1\nkind: ConfigMap\nmetadata: {{name: cfg-{k:03d}, namespace: {ns}, labels: {{bulk: \"true\"}}}}\ndata: {{key: value-{k}, region: us-east-1, owner: {ns}}}")
        docs.append(f"apiVersion: v1\nkind: Secret\nmetadata: {{name: sec-{k:03d}, namespace: {ns}, labels: {{bulk: \"true\"}}}}\ntype: Opaque\nstringData: {{token: tok-{ns}-{k}, user: svc-{k}}}")
        docs.append(f"apiVersion: v1\nkind: Service\nmetadata: {{name: svc-{k:03d}, namespace: {ns}, labels: {{bulk: \"true\"}}}}\nspec:\n  selector: {{app: app-{k % DEPLOY_PER_NS:02d}}}\n  ports: [{{port: 80, targetPort: 8080}}]")
    w(f"10-{ns}.yaml", docs)
print(f"deployments={NS*DEPLOY_PER_NS} expected_pods={total_pods}", file=sys.stderr)
