# Simulated logs / exec / port-forward for the demo cluster. @HACK@ and @PF_PORT@ are substituted by seed/demo.sh.
# Logs: kwok streams the files written by logsim.sh (CRI format). Different namespaces get different content.
apiVersion: kwok.x-k8s.io/v1alpha1
kind: ClusterLogs
metadata: {name: logs-default}
spec:
  selector:
    matchNamespaces: [shop, payments, monitoring, data, platform, kube-system, team-a, team-b]
  logs:
  - logsFile: @HACK@/.state/simlogs/app.log
    follow: true
---
apiVersion: kwok.x-k8s.io/v1alpha1
kind: ClusterLogs
metadata: {name: logs-batch}
spec:
  selector:
    matchNamespaces: [batch]
  logs:
  - logsFile: @HACK@/.state/simlogs/batch.log
    follow: true
---
apiVersion: kwok.x-k8s.io/v1alpha1
kind: ClusterLogs
metadata: {name: logs-incidents}
spec:
  selector:
    matchNamespaces: [incidents]
  logs:
  - logsFile: @HACK@/.state/simlogs/incident.log
    previousLogsFile: @HACK@/.state/simlogs/incident.previous.log
    follow: true
---
# Exec: runs the command on THIS machine as an unprivileged user (nobody) in /tmp. `kubectl exec -it POD -- sh` gives a local shell.
apiVersion: kwok.x-k8s.io/v1alpha1
kind: ClusterExec
metadata: {name: exec-all}
spec:
  execs:
  - local:
      workDir: /tmp
      envs:
      - {name: SKIFF_FAKE_POD, value: "true"}
      - {name: PS1, value: "fake-pod:\\w\\$ "}
      securityContext: {runAsUser: 65534, runAsGroup: 65534}
---
# Port-forward: every pod port is forwarded to a tiny local HTTP server on 127.0.0.1:@PF_PORT@.
apiVersion: kwok.x-k8s.io/v1alpha1
kind: ClusterPortForward
metadata: {name: portforward-all}
spec:
  forwards:
  - target: {port: @PF_PORT@, address: 127.0.0.1}
