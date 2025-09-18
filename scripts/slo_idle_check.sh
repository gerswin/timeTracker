#!/usr/bin/env bash
set -euo pipefail

# Medición rápida de SLOs en idle usando bash + curl + awk
# Uso: scripts/slo_idle_check.sh <duration_s> <interval_s> <cpu_threshold_pct> <mem_threshold_mb> [json_out]

DURATION="${1:-120}"
INTERVAL="${2:-1}"
CPU_THR="${3:-1.0}"
MEM_THR="${4:-60}"
JSON_OUT="${5:-}"

PANEL_URL=${RIPOR_PANEL:-http://127.0.0.1:49219}

build_and_run() {
  echo "[info] building agent-daemon (release)…"
  cargo build -q -p agent-daemon --release
  local exe="target/release/agent-daemon"
  echo "[info] launching agent-daemon…"
  RUST_LOG=warn "$exe" &
  AGENT_PID=$!
}

cleanup() {
  if [[ -n "${AGENT_PID:-}" ]]; then
    kill "$AGENT_PID" >/dev/null 2>&1 || true
    wait "$AGENT_PID" 2>/dev/null || true
  fi
}

wait_ready() {
  for i in {1..50}; do
    if curl -sf "$PANEL_URL/healthz" >/dev/null; then
      return 0
    fi
    sleep 0.2
  done
  echo "[error] panel no respondió" >&2
  return 1
}

percentile() {
  # args: file p
  local file="$1"; local p="$2"
  local n idx
  n=$(wc -l < "$file" | tr -d ' ')
  if [[ "$n" -le 1 ]]; then
    cat "$file" 2>/dev/null || echo 0
    return 0
  fi
  idx=$(awk -v n="$n" -v p="$p" 'BEGIN { printf("%d", (p/100.0)*(n-1) + 0.5) }')
  sort -n "$file" | awk -v idx="$idx" 'NR==idx+1 { print $1; exit }'
}

main() {
  trap cleanup EXIT
  build_and_run
  wait_ready

  echo "[info] sampling for ${DURATION}s every ${INTERVAL}s…"
  tmpdir=$(mktemp -d)
  cpu_file="$tmpdir/cpu.txt"
  mem_file="$tmpdir/mem.txt"
  end=$(( $(date +%s) + ${DURATION%.*} ))
  while [[ $(date +%s) -lt $end ]]; do
    st=$(curl -sf "$PANEL_URL/state" || true)
    if [[ -n "$st" ]]; then
      echo "$st" | sed -E -n 's/.*"cpu_pct":([0-9.\-]+).*/\1/p' >> "$cpu_file"
      echo "$st" | sed -E -n 's/.*"mem_mb":([0-9.\-]+).*/\1/p' >> "$mem_file"
    fi
    sleep "$INTERVAL"
  done

  cpu_p95=$(percentile "$cpu_file" 95)
  mem_p95=$(percentile "$mem_file" 95)
  cpu_avg=$(awk '{s+=$1} END { if (NR>0) printf("%.3f", s/NR); else print 0 }' "$cpu_file")
  mem_avg=$(awk '{s+=$1} END { if (NR>0) printf("%.3f", s/NR); else print 0 }' "$mem_file")

  cpu_ok=$(awk -v v="$cpu_p95" -v t="$CPU_THR" 'BEGIN { print (v <= t)?"true":"false" }')
  mem_ok=$(awk -v v="$mem_p95" -v t="$MEM_THR" 'BEGIN { print (v <= t)?"true":"false" }')
  pass=$(awk -v a="$cpu_ok" -v b="$mem_ok" 'BEGIN { print (a=="true" && b=="true")?"true":"false" }')

  json=$(cat <<JSON
{
  "samples": $(wc -l < "$cpu_file" | tr -d ' '),
  "interval_s": ${INTERVAL},
  "duration_s": ${DURATION},
  "cpu": {"p95": ${cpu_p95:-0}, "avg": ${cpu_avg:-0}, "threshold": ${CPU_THR}, "ok": $cpu_ok},
  "mem": {"p95": ${mem_p95:-0}, "avg": ${mem_avg:-0}, "threshold": ${MEM_THR}, "ok": $mem_ok},
  "pass": $pass
}
JSON
)
  echo "Resultados SLO (idle):"
  echo "$json"
  if [[ -n "$JSON_OUT" ]]; then
    echo "$json" > "$JSON_OUT"
  fi
  [[ "$pass" == "true" ]]
}

main "$@"
