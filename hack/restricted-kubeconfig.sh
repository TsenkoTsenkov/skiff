#!/usr/bin/env bash
# Writes a kubeconfig for the namespace-restricted `skiff-viewer` service
# account of the demo cluster (context name: restricted, namespace: shop).
#
#   hack/restricted-kubeconfig.sh /tmp/restricted.kubeconfig
#   SKIFF_TEST_RESTRICTED_KUBECONFIG=/tmp/restricted.kubeconfig \
#     cargo test -p skiff --test ui restricted
set -euo pipefail
HACK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:?usage: restricted-kubeconfig.sh <output file>}"
export KUBECONFIG="$HACK_DIR/kubeconfig"
cluster=$(kubectl config view --raw -o jsonpath='{.contexts[?(@.name=="demo")].context.cluster}')
server=$(kubectl config view --raw -o jsonpath="{.clusters[?(@.name==\"$cluster\")].cluster.server}")
ca=$(kubectl config view --raw -o jsonpath="{.clusters[?(@.name==\"$cluster\")].cluster.certificate-authority}")
token=$(kubectl --context demo -n shop create token skiff-viewer --duration=12h)
rm -f "$OUT"
export KUBECONFIG="$OUT"
kubectl config set-cluster demo --server="$server" --certificate-authority="$ca" >/dev/null
kubectl config set-credentials viewer --token="$token" >/dev/null
kubectl config set-context restricted --cluster=demo --user=viewer --namespace=shop >/dev/null
kubectl config use-context restricted >/dev/null
echo "wrote $OUT (context: restricted, namespace: shop)"
