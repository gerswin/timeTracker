#!/usr/bin/env python3
"""
Medición rápida de SLOs en idle para RiporAgent.

Mide p95 de CPU (%) y RAM (MB) del proceso del agente leyendo /state.

Uso:
  python3 scripts/slo_idle_check.py --duration 120 --interval 1.0 \
    --cpu-threshold 1.0 --mem-threshold 60

Opciones:
  --use-running    No lanza el agente; usa uno ya ejecutándose.
  --json-out PATH  Guarda resultados en JSON.
"""
import argparse
import contextlib
import json
import os
import signal
import subprocess
import sys
import time
import urllib.request

PANEL_URL = os.environ.get("RIPOR_PANEL", "http://127.0.0.1:49219")


def percentile(values, p):
    if not values:
        return 0.0
    s = sorted(values)
    k = max(0, min(len(s) - 1, int(round((p / 100.0) * (len(s) - 1)))))
    return float(s[k])


def fetch_state():
    url = f"{PANEL_URL}/state"
    with contextlib.closing(urllib.request.urlopen(url, timeout=2)) as r:
        return json.loads(r.read().decode("utf-8"))


def wait_ready(timeout=10):
    start = time.time()
    while time.time() - start < timeout:
        try:
            data = fetch_state()
            return data
        except Exception:
            time.sleep(0.2)
    raise RuntimeError("panel no respondió en tiempo")


def run_agent(release: bool):
    # Construir y ejecutar binario directamente para evitar coste de compilación en el bucle
    print("[info] construyendo agent-daemon…", flush=True)
    build_cmd = ["cargo", "build", "-q", "-p", "agent-daemon"]
    if release:
        build_cmd.append("--release")
    subprocess.check_call(build_cmd)  # requiere cargo
    exe = os.path.join("target", "release" if release else "debug", "agent-daemon" + (".exe" if os.name == "nt" else ""))
    env = os.environ.copy()
    env.setdefault("RUST_LOG", "warn")
    print("[info] lanzando agent-daemon…", flush=True)
    p = subprocess.Popen([exe], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--duration", type=float, default=120.0, help="segundos de muestreo")
    ap.add_argument("--interval", type=float, default=1.0, help="intervalo de muestreo (s)")
    ap.add_argument("--cpu-threshold", type=float, default=1.0, help="umbral CPU p95 (%)")
    ap.add_argument("--mem-threshold", type=float, default=60.0, help="umbral RAM p95 (MB)")
    ap.add_argument("--use-running", action="store_true", help="no lanzar agente, usar existente")
    ap.add_argument("--json-out", type=str, default=None)
    ap.add_argument("--debug-build", action="store_true", help="usar build debug en vez de release")
    args = ap.parse_args()

    proc = None
    try:
        if not args.use_running:
            proc = run_agent(release=(not args.debug_build))
        st = wait_ready()
        print(f"[info] device_id={st.get('device_id')} version={st.get('agent_version')}")
        print(f"[info] midiendo durante {args.duration}s con dt={args.interval}s…")

        cpu_samples = []
        mem_samples = []
        t_end = time.time() + args.duration
        while time.time() < t_end:
            try:
                st = fetch_state()
                cpu_samples.append(float(st.get("cpu_pct", 0.0)))
                mem_samples.append(float(st.get("mem_mb", 0.0)))
            except Exception as e:
                print(f"[warn] lectura fallida: {e}")
            time.sleep(args.interval)

        cpu_p95 = percentile(cpu_samples, 95)
        mem_p95 = percentile(mem_samples, 95)
        cpu_avg = sum(cpu_samples) / max(1, len(cpu_samples))
        mem_avg = sum(mem_samples) / max(1, len(mem_samples))

        cpu_ok = cpu_p95 <= args.cpu_threshold
        mem_ok = mem_p95 <= args.mem_threshold

        result = {
            "samples": len(cpu_samples),
            "interval_s": args.interval,
            "duration_s": args.duration,
            "cpu": {"p95": cpu_p95, "avg": cpu_avg, "threshold": args.cpu_threshold, "ok": cpu_ok},
            "mem": {"p95": mem_p95, "avg": mem_avg, "threshold": args.mem_threshold, "ok": mem_ok},
            "pass": bool(cpu_ok and mem_ok),
        }

        print("\nResultados SLO (idle):")
        print(json.dumps(result, indent=2))

        if args.json_out:
            with open(args.json_out, "w") as f:
                json.dump(result, f, indent=2)

        return 0 if result["pass"] else 2
    finally:
        if proc is not None:
            if os.name == "nt":
                proc.terminate()
                try:
                    proc.wait(timeout=3)
                except Exception:
                    proc.kill()
            else:
                with contextlib.suppress(Exception):
                    os.kill(proc.pid, signal.SIGTERM)
    

if __name__ == "__main__":
    sys.exit(main())
