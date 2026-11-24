# A Prometheus that Skiff finds by itself, reached through the API server's
# service proxy exactly as on a real cluster. kwok pods are not real, so
# there is nothing a real Prometheus could scrape: the Service (labelled the
# way prometheus-operator labels its governing Service, no selector) has
# hand-written Endpoints that point at hack/fake-prometheus.py on this host.
#
# The API server only proxies to an endpoint that references a Pod with the
# same IP, hence the Pod: it names a scheduler that does not exist, so
# nothing (kwok included) ever touches it, and prom_seed (hack/lib.sh)
# writes its status. @PROM_IP@ / @PROM_PORT@ are filled in there too.
apiVersion: v1
kind: Pod
metadata:
  name: fake-prometheus
  namespace: monitoring
  labels: {app.kubernetes.io/name: fake-prometheus}
  annotations: {skiff.dev/note: "Stands for hack/fake-prometheus.py on the host; see hack/seed/demo/96-prometheus.yaml.tpl"}
spec:
  schedulerName: skiff-hack-unscheduled
  containers:
  - {name: prometheus, image: "skiff.dev/hack/fake-prometheus:local"}
---
apiVersion: v1
kind: Service
metadata:
  name: prometheus-operated
  namespace: monitoring
  labels: {operated-prometheus: "true", app.kubernetes.io/name: prometheus, app.kubernetes.io/managed-by: prometheus-operator}
spec:
  clusterIP: None
  ports:
  - {name: web, port: 9090, protocol: TCP, targetPort: web}
---
apiVersion: v1
kind: Endpoints
metadata:
  name: prometheus-operated
  namespace: monitoring
  labels: {operated-prometheus: "true", app.kubernetes.io/name: prometheus}
subsets:
- addresses: [{ip: "@PROM_IP@", targetRef: {kind: Pod, namespace: monitoring, name: fake-prometheus}}]
  ports: [{name: web, port: @PROM_PORT@, protocol: TCP}]
