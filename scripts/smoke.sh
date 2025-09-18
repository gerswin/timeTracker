#!/usr/bin/env bash
set -euo pipefail

# Load local env if present
if [ -f .env ]; then
  set -a
  . ./.env
  set +a
fi

# Derive base URL from PANEL_ADDR or default
PANEL_ADDR="${PANEL_ADDR:-127.0.0.1:49219}"
BASE="http://${PANEL_ADDR}"

echo "[smoke] building…"
cargo build -q -p agent-daemon

echo "[smoke] launching… (IDLE_ACTIVE_THRESHOLD_MS=3000, PANEL_ADDR=${PANEL_ADDR})"
IDLE_ACTIVE_THRESHOLD_MS=3000 RUST_LOG=info PANEL_ADDR="${PANEL_ADDR}" target/debug/agent-daemon &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT

for i in {1..30}; do
  curl -sf "${BASE}/healthz" >/dev/null && break
  sleep 0.2
done

echo "[smoke] /healthz:"; curl -s "${BASE}/healthz"; echo
STATE_JSON=$(curl -s "${BASE}/state")
echo "[smoke] /state:"; echo "$STATE_JSON"

# Validación básica de ONLINE_ACTIVE/ONLINE_IDLE con umbral corto
if command -v python3 >/dev/null 2>&1; then
  ACT1=$(printf '%s' "$STATE_JSON" | python3 -c 'import sys, json; d=json.load(sys.stdin); print(d.get("activity_state",""))')
  IDLE1=$(printf '%s' "$STATE_JSON" | python3 -c 'import sys, json; d=json.load(sys.stdin); print(d.get("input_idle_ms",0))')
  echo "[smoke] activity_state(1)=$ACT1 input_idle_ms(1)=$IDLE1"
  if [ "$ACT1" != "ONLINE_ACTIVE" ]; then
    echo "[smoke][WARN] estado inicial no es ONLINE_ACTIVE (puede ser esperado si no hay input reciente)"
  fi
  # Espera a que pase el umbral (3s) sin input y revalida
  sleep 5
  STATE_JSON2=$(curl -s "${BASE}/state")
  ACT2=$(printf '%s' "$STATE_JSON2" | python3 -c 'import sys, json; d=json.load(sys.stdin); print(d.get("activity_state",""))')
  IDLE2=$(printf '%s' "$STATE_JSON2" | python3 -c 'import sys, json; d=json.load(sys.stdin); print(d.get("input_idle_ms",0))')
  echo "[smoke] activity_state(2)=$ACT2 input_idle_ms(2)=$IDLE2"
  if [ "$ACT2" != "ONLINE_IDLE" ]; then
    if [ "${SMOKE_STRICT_IDLE:-0}" = "1" ]; then
      echo "[smoke][FAIL] no se observó ONLINE_IDLE tras el umbral" >&2
      exit 1
    else
      echo "[smoke][WARN] no se observó ONLINE_IDLE (quizá hubo input); continúa"
    fi
  else
    echo "[smoke][OK] cambio a ONLINE_IDLE detectado"
  fi
else
  echo "[smoke] python3 no disponible; se omite validación de activity_state"
fi
echo "[smoke] /queue:"; curl -s "${BASE}/queue"; echo

echo "[smoke] /permissions (macOS only):"
if curl -sf "${BASE}/permissions" >/dev/null; then
  PERMS_JSON=$(curl -s "${BASE}/permissions")
  echo "$PERMS_JSON"
  # Si ambos permisos están concedidos, validamos /debug/sample y que title_source != none
  if command -v python3 >/dev/null 2>&1; then
    # Espera corta por auto-prompt + re-chequeo (hasta ~10s)
    TRIES=5
    BOTH_OK="false"
    for i in $(seq 1 $TRIES); do
      BOTH_OK=$(printf '%s' "$PERMS_JSON" | python3 -c 'import sys, json; d=json.load(sys.stdin); print("true" if bool(d.get("accessibility_ok")) and bool(d.get("screen_recording_ok")) else "false")')
      if [ "$BOTH_OK" = "true" ]; then
        break
      fi
      sleep 2
      PERMS_JSON=$(curl -s "${BASE}/permissions")
    done
    if [ "$BOTH_OK" = "true" ]; then
      echo "[smoke] /debug/sample:"
      SAMPLE=$(curl -s "${BASE}/debug/sample")
      echo "$SAMPLE"
      TITLE_SRC=$(printf '%s' "$SAMPLE" | python3 -c 'import sys, json;\
import sys, json\
\
try:\
    d=json.load(sys.stdin); print(d.get("title_source",""))\
except Exception:\
    print("")')
      if [ "$TITLE_SRC" = "none" ] || [ -z "$TITLE_SRC" ]; then
        echo "[smoke][FAIL] title_source es '$TITLE_SRC' pese a permisos concedidos" >&2
        exit 1
      else
        echo "[smoke][OK] title_source='$TITLE_SRC' con permisos concedidos"
      fi
    else
      echo "[smoke] permisos incompletos; se omite aserción de /debug/sample"
    fi
  else
    echo "[smoke] python3 no disponible; se omite validación de /debug/sample"
  fi
else
  echo "(fallback) perms in /state:"
  curl -s "${BASE}/state" | sed -n 's/.*"perms":\(\{[^}]*\}\).*/\1/p'; echo || true
fi

echo "[smoke] done"
