#!/usr/bin/env bash
# Seeds the "demo" cluster. Idempotent (kubectl apply). Usage: seed/demo.sh
source "$(dirname "$0")/../lib.sh"; source "$HACK_DIR/seed/common.sh"
CTX=demo
S="$HACK_DIR/seed"

log "demo: kwok stages (node/pod lifecycle, simulated unhealthy states, metrics)"
python3 -I "$S/kwok/gen-sim-stages.py" > "$S/kwok/40-sim-pod-states.yaml"
apply_stages $CTX

log "demo: simulated logs / exec / port-forward"
sed -e "s|@HACK@|$HACK_DIR|g" -e "s|@PF_PORT@|$PF_TARGET_PORT|g" "$S/kwok/50-debug-endpoints.yaml.tpl" | kc $CTX apply -f - >/dev/null

log "demo: nodes"
node() {  # name role zone itype cpu mem [extra-labels] [taints] [unschedulable] [extra-capacity]
  local n="$1" role="$2" zone="$3" it="$4" cpu="$5" mem="$6" xl="${7:-}" taints="${8:-}" unsched="${9:-}" xcap="${10:-}"
  local rl=""; [ -n "$role" ] && rl="\"node-role.kubernetes.io/$role\": \"\","
  cat <<YAML
---
apiVersion: v1
kind: Node
metadata:
  name: $n
  annotations: {kwok.x-k8s.io/node: fake, "metrics.k8s.io/resource-metrics-path": "/metrics/nodes/$n/metrics/resource", node.alpha.kubernetes.io/ttl: "0"}
  labels: {type: kwok, "kubernetes.io/hostname": $n, "kubernetes.io/os": linux, "kubernetes.io/arch": amd64, $rl "topology.kubernetes.io/zone": $zone, "topology.kubernetes.io/region": ${zone%?}, "node.kubernetes.io/instance-type": $it $xl}
spec:
  providerID: kwok://$n${taints:+
  taints: $taints}${unsched:+
  unschedulable: true}
status:
  allocatable: {cpu: "$cpu", memory: $mem, pods: "110", ephemeral-storage: 200Gi$xcap}
  capacity: {cpu: "$cpu", memory: $mem, pods: "110", ephemeral-storage: 200Gi$xcap}
  nodeInfo: {kubeletVersion: v1.37.2, osImage: "Ubuntu 24.04.1 LTS", kernelVersion: 6.8.0-1015-aws, containerRuntimeVersion: "containerd://1.7.18", architecture: amd64, operatingSystem: linux}
YAML
}
{
  node demo-control-plane  control-plane us-east-1a m5.xlarge  8 32Gi "" '[{key: node-role.kubernetes.io/control-plane, effect: NoSchedule}]'
  node demo-worker-1 worker us-east-1a m5.2xlarge 16 64Gi
  node demo-worker-2 worker us-east-1b m5.2xlarge 16 64Gi
  node demo-worker-3 worker us-east-1c m5.2xlarge 16 64Gi
  node demo-worker-4 worker us-east-1a c5.4xlarge 16 32Gi ", pool: compute"
  node demo-worker-5 worker us-east-1b m5.2xlarge 16 64Gi ", note: will-be-notready"
  node demo-worker-6 worker us-east-1c m5.2xlarge 16 64Gi ", note: cordoned-for-maintenance" "" yes
  node demo-gpu-1    gpu    us-east-1b p4d.24xlarge 48 384Gi ", accelerator: nvidia-a100" '[{key: nvidia.com/gpu, value: present, effect: NoSchedule}]' "" ', nvidia.com/gpu: "2"'
  node demo-highmem-1 worker us-east-1c r5.8xlarge 32 256Gi ", workload: memory" '[{key: dedicated, value: analytics, effect: NoSchedule}]'
  node demo-spot-1   worker us-east-1a m5.large 4 16Gi ", \"node.kubernetes.io/lifecycle\": spot, disktype: ssd" '[{key: spot, value: "true", effect: PreferNoSchedule}]'
} | kc $CTX apply -f - >/dev/null
wait_for_nodes_ready $CTX 10 120

log "demo: namespaces, storage, CRDs, RBAC"
kc $CTX apply -f "$S/demo/00-namespaces.yaml" >/dev/null
kc $CTX apply -f "$S/demo/01-storage.yaml" >/dev/null
kc $CTX apply -f "$S/demo/02-crds.yaml" >/dev/null
kc $CTX apply -f "$S/demo/03-rbac.yaml" >/dev/null
kc $CTX wait --for condition=established --timeout=60s crd/databases.stack.skiff.dev crd/certificates.pki.skiff.dev crd/backuppolicies.ops.skiff.dev >/dev/null

log "demo: TLS secrets (self-signed)"
if command -v openssl >/dev/null; then
  T="$(mktemp -d)"
  for pair in shop:shop-tls-secret:shop.example.com monitoring:grafana-tls-secret:grafana.example.com; do
    IFS=: read -r ns sn cn <<<"$pair"
    openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=$cn" -keyout "$T/k.pem" -out "$T/c.pem" >/dev/null 2>&1
    kc $CTX -n "$ns" create secret tls "$sn" --cert "$T/c.pem" --key "$T/k.pem" --dry-run=client -o yaml | kc $CTX apply -f - >/dev/null
  done
  rm -rf "$T"
fi

log "demo: workloads"
kc $CTX apply -f "$S/demo/10-workloads.yaml" >/dev/null

log "demo: payments/ledger rollout scenario (v1 healthy, then v2 stuck in ImagePullBackOff)"
if ! kc $CTX -n payments get deploy ledger >/dev/null 2>&1; then
  sed -e 's|@STATELABEL@||' -e 's|@VERSION@|1.4.2|' "$S/demo/ledger.yaml" | kc $CTX apply -f - >/dev/null
  kc $CTX -n payments annotate deploy ledger kubernetes.io/change-cause="initial release 1.4.2" --overwrite >/dev/null
  kc $CTX -n payments rollout status deploy/ledger --timeout=120s >/dev/null || log "WARN: ledger v1 not ready in time"
  sed -e 's|@STATELABEL@|, "sim.skiff.dev/state": imagepull|' -e 's|@VERSION@|2.0.0-rc1|' "$S/demo/ledger.yaml" | kc $CTX apply -f - >/dev/null
  kc $CTX -n payments annotate deploy ledger kubernetes.io/change-cause="upgrade to 2.0.0-rc1 (bad image tag)" --overwrite >/dev/null
fi

log "demo: custom resources + events"
kc $CTX apply -f "$S/demo/90-custom-resources.yaml" >/dev/null
kc $CTX apply -f "$S/demo/95-events.yaml" >/dev/null

log "demo: NotReady node (kwok chaos stage keyed on a label)"
kc $CTX label node demo-worker-5 node-not-ready.stage.kwok.x-k8s.io=true --overwrite >/dev/null
kc $CTX annotate node demo-worker-5 node-not-ready.stage.kwok.x-k8s.io/reason=KubeletNotReady node-not-ready.stage.kwok.x-k8s.io/message="container runtime network not ready: cni plugin not initialized" --overwrite >/dev/null
log "demo: seeded"
