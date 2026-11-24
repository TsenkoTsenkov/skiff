#!/usr/bin/env bash
# Measures what a desktop Kubernetes client costs the machine it runs on.
# The same procedure for any app: Skiff, another desktop client, a terminal
# client (in a terminal emulator), anything with a launch command. See
# README.md.
#
#   hack/bench/run.sh app --label "Name 1.2.3" [options] -- <launch command>
#       Launches the app, waits until its pod list is showing (you press
#       Enter, unless --ready-regex or --ready-after says how to tell), then
#       measures: memory of the whole process tree (PSS and RSS), process
#       and thread counts, CPU over 60 idle seconds, listening sockets, the
#       remote addresses it connected to in its first 60 s, and the size on
#       disk of each --installed path. With --window-runs N it first times
#       N launches until a window is visible. Writes
#       <out>/<slug>.{steady,size,startup-window}.json and prints a table
#       row; results.py puts the numbers into a results file for chart.py.
#
#   hack/bench/run.sh skiff [--binary PATH] [--runs N] [--context NAME]
#       The whole Skiff procedure against the local test cluster
#       (hack/cluster-up.sh): launch timings over N runs (warm, and cold
#       when root), the steady-state numbers above, a syscall trace of every
#       connection, binary and package sizes.
#
#   hack/bench/run.sh row <out>/<slug>.steady.json [<out>/<slug>.size.json]
#       Prints the table row again from saved results.
#
# Options for "app" (and "skiff"):
#   --label TEXT        name and version of what is measured (app: required)
#   --installed PATH    a file or directory that is the installed app; repeat
#   --ready-regex RE    ready when the app prints a line matching RE
#   --ready-after SEC   ready SEC seconds after launch (no person needed, but
#                       only honest if the list really is loaded by then:
#                       check the screenshot)
#   --idle SEC          length of the idle CPU window (default 60)
#   --window-runs N     app: before the steady run, launch N times and time
#                       each until a window of the app is visible on the X
#                       display (needs xdotool); the app is closed each time
#   --xvfb              run on a private virtual display (Xvfb) instead of
#                       the current one; implied when DISPLAY is not set
#   --trace             also run once more under strace and list every
#                       connect/bind/listen/exec (Linux)
#   --env K=V           set a variable for the app; repeat
#   --unset K           remove a variable for the app; repeat
#   --fresh-state       empty XDG config/data/state directories for the app
#   --out DIR           where results go (default target/bench/<date>)
#
# Linux is the tested platform. On macOS "app" works through ps, footprint
# and lsof (no --xvfb, no --trace) and is UNTESTED.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
measure=(python3 "$here/measure.py")

die() {
  echo "run.sh: $*" >&2
  exit 1
}

mode="${1:-}"
[ -n "$mode" ] || die "say what to do: app, skiff or row (see the top of this file)"
shift

label=""
out=""
runs=7
window_runs=0
idle=60
context=scale
binary=""
xvfb=0
trace=0
cold=1
ready=()
installed=()
pass=() # options handed to measure.py as they are
cmd=()
while [ $# -gt 0 ]; do
  case "$1" in
    --label) label="$2"; shift ;;
    --out) out="$2"; shift ;;
    --runs) runs="$2"; shift ;;
    --idle) idle="$2"; shift ;;
    --window-runs) window_runs="$2"; shift ;;
    --context) context="$2"; shift ;;
    --binary) binary="$2"; shift ;;
    --installed) installed+=("$2"); shift ;;
    --ready-regex | --ready-after) ready=("$1" "$2"); shift ;;
    --env | --unset) pass+=("$1" "$2"); shift ;;
    --fresh-state) pass+=("$1") ;;
    --xvfb) xvfb=1 ;;
    --trace) trace=1 ;;
    --no-cold) cold=0 ;;
    --) shift; cmd=("$@"); break ;;
    *)
      if [ "$mode" = row ]; then cmd+=("$1"); else die "unknown option $1"; fi
      ;;
  esac
  shift
done

if [ "$mode" = row ]; then
  exec "${measure[@]}" row "${cmd[@]}"
fi

out="${out:-$repo/target/bench/$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$out"
slug() { printf '%s' "$1" | tr -cs 'A-Za-z0-9._-' '-' | sed 's/^-//; s/-$//'; }

# A private X display, so the numbers do not depend on a desktop session
# and nothing else draws. The X server is NOT part of the measured tree.
xvfb_pid=""
start_xvfb() {
  command -v Xvfb >/dev/null || die "--xvfb needs Xvfb (apt install xvfb)"
  local n
  for n in $(seq 90 120); do
    [ -e "/tmp/.X11-unix/X$n" ] && continue
    Xvfb ":$n" -screen 0 1440x900x24 -nolisten tcp >"$out/xvfb.log" 2>&1 &
    xvfb_pid=$!
    local i
    for i in $(seq 50); do
      [ -e "/tmp/.X11-unix/X$n" ] && break
      sleep 0.1
    done
    kill -0 "$xvfb_pid" 2>/dev/null || continue
    export DISPLAY=":$n"
    unset WAYLAND_DISPLAY
    return
  done
  die "could not start Xvfb"
}
cleanup() {
  [ -n "$xvfb_pid" ] && kill "$xvfb_pid" 2>/dev/null
  return 0
}
trap cleanup EXIT

if [ "$(uname)" = Linux ]; then
  if [ "$xvfb" = 1 ] || { [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; }; then
    start_xvfb
  fi
elif [ "$xvfb" = 1 ] || [ "$trace" = 1 ]; then
  die "--xvfb and --trace are Linux only"
fi

# What draws: worth a line in every result, because a software rasterizer
# changes the memory and CPU of anything that renders with the GPU.
describe_machine() {
  {
    echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "kernel: $(uname -srm)"
    if [ "$(uname)" = Linux ]; then
      echo "os: $(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}")"
      echo "cpu: $(nproc) x $(sed -n 's/^model name[^:]*: //p' /proc/cpuinfo | head -1)"
      echo "ram: $(awk '/MemTotal/ {printf "%.1f GiB", $2/1048576}' /proc/meminfo)"
      echo "display: ${DISPLAY:-}${WAYLAND_DISPLAY:+ wayland $WAYLAND_DISPLAY}${xvfb_pid:+ (Xvfb 1440x900x24)}"
      if command -v vulkaninfo >/dev/null; then
        echo "vulkan devices:"
        vulkaninfo --summary 2>/dev/null | grep -E 'deviceName|deviceType|driverInfo' | sed 's/^ */  /'
      else
        echo "vulkan devices: unknown (install vulkan-tools to record them)"
      fi
    else
      echo "os: $(sw_vers -productName 2>/dev/null) $(sw_vers -productVersion 2>/dev/null)"
      echo "cpu: $(sysctl -n hw.ncpu) x $(sysctl -n machdep.cpu.brand_string 2>/dev/null)"
      echo "ram: $(($(sysctl -n hw.memsize) / 1073741824)) GiB"
    fi
  } >"$out/machine.txt"
}

steady_and_size() { # label, extra measure.py options..., then -- command
  local label="$1" s
  shift
  s="$(slug "$label")"
  "${measure[@]}" steady --label "$label" --idle "$idle" --out "$out/$s.steady.json" --log "$out/$s.app.log" \
    --screenshot "$out/$s.png" "$@"
  local files=("$out/$s.steady.json")
  if [ ${#installed[@]} -gt 0 ]; then
    "${measure[@]}" size --label "$label" --out "$out/$s.size.json" "${installed[@]}"
    files+=("$out/$s.size.json")
  fi
  echo
  echo "Row for docs/benchmarks.md (results in $out):"
  "${measure[@]}" row "${files[@]}" | tee "$out/$s.row.md"
}

case "$mode" in
  app)
    [ -n "$label" ] || die "app needs --label \"Name version\""
    [ ${#cmd[@]} -gt 0 ] || die "give the launch command after --"
    describe_machine
    if [ "$window_runs" -gt 0 ]; then
      # Until a window exists, whatever it shows: a splash screen counts.
      # The first launch warms the page cache and is not timed.
      echo "== one untimed launch, then time to a visible window, $window_runs runs" >&2
      "${measure[@]}" startup --label "$label" --runs 1 --window ${pass[@]+"${pass[@]}"} \
        --out "$out/$(slug "$label").startup-warmup.json" -- "${cmd[@]}"
      "${measure[@]}" startup --label "$label" --runs "$window_runs" --window ${pass[@]+"${pass[@]}"} \
        --out "$out/$(slug "$label").startup-window.json" -- "${cmd[@]}"
    fi
    steady_and_size "$label" ${ready[@]+"${ready[@]}"} ${pass[@]+"${pass[@]}"} -- "${cmd[@]}"
    if [ "$trace" = 1 ]; then
      echo "Now once more under strace (60 s; bring the app to its pod list again if you like)." >&2
      "${measure[@]}" trace --label "$label" --out "$out/$(slug "$label").trace.json" \
        --log "$out/$(slug "$label").strace.log" ${pass[@]+"${pass[@]}"} -- "${cmd[@]}"
    fi
    ;;

  skiff)
    binary="${binary:-$repo/target/release/skiff}"
    [ -x "$binary" ] || die "$binary is not there: cargo build --release -p skiff"
    kubeconfig="${SKIFF_KUBECONFIG:-$repo/hack/kubeconfig}"
    [ -f "$kubeconfig" ] || die "$kubeconfig is not there: hack/cluster-up.sh"
    label="${label:-Skiff $("$binary" --version | sed 's/^skiff //')}"
    s="$(slug "$label")"
    describe_machine
    {
      echo "skiff: $("$binary" --version)"
      echo "commit: $(git -C "$repo" rev-parse HEAD) ($(git -C "$repo" status --porcelain -uno | grep -q . && echo "with uncommitted changes" || echo clean))"
      echo "cluster: context $context, $(kubectl --kubeconfig "$kubeconfig" --context "$context" get nodes --no-headers 2>/dev/null | wc -l) nodes, $(kubectl --kubeconfig "$kubeconfig" --context "$context" get pods -A --no-headers 2>/dev/null | wc -l) pods"
    } >>"$out/machine.txt" 2>/dev/null || true
    cat "$out/machine.txt"

    # The test bed's churn loop edits the demo cluster every few seconds;
    # stopped while measuring so that "idle" is idle. Started again after.
    churn_was_running=0
    if [ -x "$repo/hack/churn.sh" ] && "$repo/hack/churn.sh" status 2>/dev/null | grep -qi "running" &&
      ! "$repo/hack/churn.sh" status 2>/dev/null | grep -qi "not running"; then
      churn_was_running=1
      "$repo/hack/churn.sh" stop >/dev/null 2>&1 || true
    fi
    restart_churn() {
      [ "$churn_was_running" = 1 ] && "$repo/hack/churn.sh" start >/dev/null 2>&1
      cleanup
    }
    trap restart_churn EXIT

    # No update check in the timed and steady runs: it starts curl, which
    # would count as a second process and a second destination. The trace
    # runs below show exactly what the check does when it is on.
    base=(--env "KUBECONFIG=$kubeconfig" --env SKIFF_TRACE_STARTUP=1 --fresh-state)
    quiet=(--env SKIFF_NO_UPDATE_CHECK=1)
    app=("$binary" --context "$context" --view pods)
    marks=(--mark 'first_frame=startup: first frame built at (?P<self>\S+)' --mark 'pods_listed=startup: \d+ rows live at (?P<self>\S+)')

    echo "== launch timings, warm, $runs runs" >&2
    "${measure[@]}" startup --label "$label" --runs "$runs" "${marks[@]}" "${base[@]}" "${quiet[@]}" \
      --out "$out/$s.startup-warm.json" --log "$out/$s.startup-warm.log" -- "${app[@]}"
    echo "== time to a visible window, warm, $runs runs" >&2
    "${measure[@]}" startup --label "$label" --runs "$runs" --window "${marks[@]}" "${base[@]}" "${quiet[@]}" \
      --out "$out/$s.startup-window.json" -- "${app[@]}"
    if [ "$cold" = 1 ] && [ -w /proc/sys/vm/drop_caches ]; then
      echo "== launch timings, cold (page cache dropped before each), $runs runs" >&2
      "${measure[@]}" startup --label "$label" --runs "$runs" --drop-caches --pause 3 "${marks[@]}" "${base[@]}" "${quiet[@]}" \
        --out "$out/$s.startup-cold.json" -- "${app[@]}"
    else
      echo "== cold launch timings skipped (need root to drop the page cache, or --no-cold given)" >&2
    fi

    echo "== steady state: memory, processes, threads, idle CPU, sockets" >&2
    installed=("$binary")
    # (Packages: hack/linux-package.sh --binary target/release/skiff [--out target/dist])
    for d in "$repo/dist" "$repo/target/dist"; do
      for f in "$d"/skiff-linux-*.tar.gz "$d"/skiff_*.deb "$d"/Skiff-linux-*.AppImage; do
        [ -e "$f" ] && installed+=("$f")
      done
    done
    steady_and_size "$label" --ready-regex 'startup: \d+ rows live' "${base[@]}" "${quiet[@]}" -- "${app[@]}"

    echo "== steady state again with the update check on (the default)" >&2
    "${measure[@]}" steady --label "$label, update check on" --idle "$idle" --ready-regex 'startup: \d+ rows live' \
      "${base[@]}" --out "$out/$s.steady-update.json" -- "${app[@]}"

    if command -v strace >/dev/null; then
      echo "== every connection, 60 s under strace: update check off, then on" >&2
      "${measure[@]}" trace --label "$label, no update check" "${base[@]}" "${quiet[@]}" \
        --out "$out/$s.trace-quiet.json" --log "$out/$s.strace-quiet.log" -- "${app[@]}"
      "${measure[@]}" trace --label "$label, update check on" "${base[@]}" \
        --out "$out/$s.trace-update.json" --log "$out/$s.strace-update.log" -- "${app[@]}"
    fi
    echo "Results are in $out" >&2
    ;;

  *) die "unknown mode $mode (app, skiff or row)" ;;
esac
