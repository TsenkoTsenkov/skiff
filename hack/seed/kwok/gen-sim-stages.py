#!/usr/bin/env python3
"""Generate kwok Stage CRs that simulate unhealthy pod states.

A pod opts in with the label  sim.skiff.dev/state=<state>.
Optional annotation sim.skiff.dev/restarts overrides the restart count.
"""
import sys

HEAD = """apiVersion: kwok.x-k8s.io/v1alpha1
kind: Stage
metadata:
  name: sim-pod-{state}
spec:
  resourceRef:
    apiGroup: v1
    kind: Pod
  selector:
    matchExpressions:
    - key: '.metadata.labels["sim.skiff.dev/state"]'
      operator: 'In'
      values:
      - '{state}'
    - key: '.metadata.deletionTimestamp'
      operator: 'DoesNotExist'
    - key: '.status.phase'
      operator: 'In'
      values:
      - 'Pending'
    - key: '.status.conditions.[] | select( .type == "Initialized" ) | .status'
      operator: 'In'
      values:
      - 'True'
    - key: '.status.containerStatuses.[].state.waiting.reason'
      operator: 'In'
      values:
      - 'ContainerCreating'
      - 'PodInitializing'
  weight: 10000
  delay:
    durationMilliseconds: 1500
    jitterDurationMilliseconds: 3000
  next:
    event:
      type: {etype}
      reason: {ereason}
      message: {emsg}
    statusTemplate: |
      {{{{ $now := Now }}}}
      {{{{ $restarts := or ( index ( or .metadata.annotations dict ) "sim.skiff.dev/restarts" ) "{restarts}" }}}}
      conditions:
      - lastProbeTime: null
        lastTransitionTime: {{{{ $now | Quote }}}}
        status: "{ready}"
        reason: {creason}
        type: Ready
      - lastProbeTime: null
        lastTransitionTime: {{{{ $now | Quote }}}}
        status: "{ready}"
        reason: {creason}
        type: ContainersReady
      containerStatuses:
      {{{{ range $index, $item := .spec.containers }}}}
      - image: {{{{ $item.image | Quote }}}}
        name: {{{{ $item.name | Quote }}}}
        ready: false
        started: {started}
        restartCount: {{{{ $restarts }}}}
{cstate}
      {{{{ end }}}}
      hostIP: {{{{ NodeIPWith .spec.nodeName | Quote }}}}
      podIP: {{{{ PodIPWith .spec.nodeName ( or .spec.hostNetwork false ) ( or .metadata.uid "" ) ( or .metadata.name "" ) ( or .metadata.namespace "" ) | Quote }}}}
      phase: {phase}
      startTime: {{{{ $now | Quote }}}}
"""

STARTED = {"notready": "true"}

def waiting(reason, msg):
    return f"""        state:
          waiting:
            reason: {reason}
            message: {msg}"""

STATES = {
 "crashloop": dict(etype="Warning", ereason="BackOff", emsg="Back-off restarting failed container", restarts=7, ready="False", creason="ContainersNotReady", phase="Running",
   cstate="""        lastState:
          terminated:
            exitCode: 1
            reason: Error
            startedAt: {{ $now | Quote }}
            finishedAt: {{ $now | Quote }}
""" + waiting("CrashLoopBackOff", '"back-off 5m0s restarting failed container"')),
 "oomkilled": dict(etype="Warning", ereason="BackOff", emsg="Back-off restarting failed container (OOMKilled)", restarts=12, ready="False", creason="ContainersNotReady", phase="Running",
   cstate="""        lastState:
          terminated:
            exitCode: 137
            reason: OOMKilled
            startedAt: {{ $now | Quote }}
            finishedAt: {{ $now | Quote }}
""" + waiting("CrashLoopBackOff", '"back-off 5m0s restarting failed container"')),
 "imagepull": dict(etype="Warning", ereason="Failed", emsg="Failed to pull image: rpc error - code = NotFound desc = manifest unknown", restarts=0, ready="False", creason="ContainersNotReady", phase="Pending",
   cstate=waiting("ImagePullBackOff", '"Back-off pulling image"')),
 "errimagepull": dict(etype="Warning", ereason="Failed", emsg="Error ErrImagePull - pull access denied, repository does not exist or may require authorization", restarts=0, ready="False", creason="ContainersNotReady", phase="Pending",
   cstate=waiting("ErrImagePull", '"pull access denied, repository does not exist or may require authorization"')),
 "configerror": dict(etype="Warning", ereason="Failed", emsg='Error - couldn&apos;t find key DATABASE_URL in Secret', restarts=0, ready="False", creason="ContainersNotReady", phase="Pending",
   cstate=waiting("CreateContainerConfigError", '"secret \\"app-secrets\\" not found"')),
 "error": dict(etype="Warning", ereason="Failed", emsg="Container exited with a non-zero status", restarts=0, ready="False", creason="PodFailed", phase="Failed",
   cstate="""        state:
          terminated:
            exitCode: 1
            reason: Error
            message: "task failed: upstream returned 503"
            startedAt: {{ $now | Quote }}
            finishedAt: {{ $now | Quote }}"""),
 "notready": dict(etype="Warning", ereason="Unhealthy", emsg="Readiness probe failed: HTTP probe failed with statuscode: 503", restarts=0, ready="False", creason="ContainersNotReady", phase="Running",
   cstate="""        state:
          running:
            startedAt: {{ $now | Quote }}"""),
}
out=[]
for state,p in STATES.items():
    p=dict(p); p["state"]=state; p["started"]=STARTED.get(state,"false")
    p["emsg"]=p["emsg"].replace("&apos;","'")
    p["emsg"]='"'+p["emsg"].replace('"','\\"')+'"'
    p["ereason"]=p["ereason"]
    out.append(HEAD.format(**p))
sys.stdout.write("---\n".join(out))
