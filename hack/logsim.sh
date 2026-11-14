#!/usr/bin/env bash
# Appends CRI-format log lines to the files the demo cluster's ClusterLogs resources serve.
# kwok serves these files for `kubectl logs [-f]` on every fake pod in the matching namespaces.
source "$(dirname "$0")/lib.sh"
DIR="$SIM_DIR"
ts() { date -u +%Y-%m-%dT%H:%M:%S.%NZ; }
paths=(/api/v1/products /api/v1/cart /api/v1/orders /healthz /api/v1/users/me /static/app.js /metrics)
methods=(GET GET GET POST PUT GET)
cri() { printf '%s stdout F %s\n' "$(ts)" "$1"; }
crie() { printf '%s stderr F %s\n' "$(ts)" "$1"; }

app_line() {
  local r=$((RANDOM % 100)) p=${paths[RANDOM % ${#paths[@]}]} m=${methods[RANDOM % ${#methods[@]}]}
  if   [ $r -lt 80 ]; then cri "level=info msg=\"request\" method=$m path=$p status=200 duration_ms=$((RANDOM % 90 + 1)) bytes=$((RANDOM % 9000))"
  elif [ $r -lt 90 ]; then cri "level=info msg=\"request\" method=$m path=$p status=304 duration_ms=$((RANDOM % 9 + 1))"
  elif [ $r -lt 95 ]; then cri "level=warn msg=\"slow request\" method=$m path=$p status=200 duration_ms=$((RANDOM % 2000 + 900))"
  elif [ $r -lt 98 ]; then crie "level=error msg=\"upstream timeout\" path=$p status=504 err=\"context deadline exceeded\""
  else cri "{\"ts\":\"$(ts)\",\"level\":\"info\",\"msg\":\"json structured line\",\"user\":\"u$((RANDOM % 500))\",\"cart_items\":$((RANDOM % 9))}"
  fi
}
batch_line() { cri "[$(date -u +%H:%M:%S)] processed batch=$((RANDOM % 10000)) rows=$((RANDOM % 50000)) elapsed=$((RANDOM % 900))ms"; }
incident_line() {
  case $((RANDOM % 4)) in
    0) crie "panic: runtime error: invalid memory address or nil pointer dereference";;
    1) crie "level=fatal msg=\"failed to connect to database\" err=\"dial tcp 10.96.14.20:5432: connect: connection refused\"";;
    2) cri "level=info msg=\"starting server\" addr=:8080";;
    3) crie "goroutine 1 [running]: main.main() /src/main.go:42 +0x1d4";;
  esac
}

# seed initial history (so `kubectl logs --tail`/`--since` have content immediately)
if [ ! -s "$DIR/app.log" ]; then
  for _ in $(seq 1 300); do app_line; done  > "$DIR/app.log"
  for _ in $(seq 1 100); do batch_line; done > "$DIR/batch.log"
  for _ in $(seq 1 40); do incident_line; done > "$DIR/incident.log"
  for _ in $(seq 1 30); do incident_line; done > "$DIR/incident.previous.log"
fi

while true; do
  for _ in $(seq 1 $((RANDOM % 3 + 1))); do app_line >> "$DIR/app.log"; done
  [ $((RANDOM % 4)) -eq 0 ] && batch_line >> "$DIR/batch.log"
  [ $((RANDOM % 5)) -eq 0 ] && incident_line >> "$DIR/incident.log"
  # cap file size by in-place truncation of the oldest lines (keep newest 3000)
  for f in app batch incident; do
    if [ "$(stat -c %s "$DIR/$f.log")" -gt 1500000 ]; then tail -n 3000 "$DIR/$f.log" > "$DIR/$f.log.tmp" && cat "$DIR/$f.log.tmp" > "$DIR/$f.log"; rm -f "$DIR/$f.log.tmp"; fi
  done
  sleep 1
done
