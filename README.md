# RiporAgent — Agente Rust Multiplataforma (Base 0)

Proyecto del agente de transparencia y uso para Windows 10/11, macOS 12+ y Ubuntu 20.04+.

## Quick Start
- macOS (.app bundle):
  - Empaqueta: `APP_VERSION=0.1.1 BUNDLE_ID=com.ripor.Ripor HELPER_BUNDLE_ID=com.ripor.Ripor.LoginItem bash scripts/macos_pack.sh`
  - Abre la app: `open dist/Ripor.app` (la UI de barra iniciará el `agent-daemon`).
  - Verifica: `curl http://127.0.0.1:49219/healthz` y abre `http://127.0.0.1:49219/ui`.
  - Login Item: usa el menú “Iniciar al abrir sesión”. Estado: `./target/release/agent-ui-macos --print-login-state` → `{ "sm_registered": true|false, "launchagent_present": true|false }`.
  - Permisos: desde `http://127.0.0.1:49219/ui` pulsa “Solicitar permisos” u “Abrir Accesibilidad/Screen Recording”.

- Windows (tray UI):
  - Arranca el daemon: `set RUST_LOG=info && cargo run -p agent-daemon`.
  - Inicia la bandeja: `cargo run -p agent-ui-windows --release`.
  - Menú: “Ver panel” abre `http://127.0.0.1:49219/ui`, “Pausar/Reanudar”, “Iniciar al abrir sesión” (autorun en `HKCU\...\Run`).
  - Icono: coloca `assets/icons/windows/icon.ico` (se incrusta en build). Usa `RIPOR_NO_EMBED_ICON=1` para omitir.

- Variables útiles:
  - `PANEL_ADDR=127.0.0.1:49219` (bind del panel), `IDLE_ACTIVE_THRESHOLD_MS=60000`, `RIPOR_NO_AUTO_PROMPT=1`.

## Estado actual (Fase 0)
- Workspace Rust creado: `agent-core` (lib) y `agent-daemon` (bin).
- Panel local en `http://127.0.0.1:49219` con endpoints mínimos.
- Cola SQLite cifrada (AES‑256‑GCM + Zstd) y `deviceId` persistente.
- Métricas locales (CPU/Mem) y logging (`tracing`).

Consulta el progreso en `plan.md` (marcado por fases y DoD).

## Requisitos
- Rust 1.75+ (estable) y `cargo`.
- macOS 12+, Windows 10/11, Ubuntu 20.04+ (para ejecución nativa).

## Construcción
```
cargo build -p agent-daemon
```

## Ejecución local (panel HTTP)
```
RUST_LOG=info cargo run -p agent-daemon
# Abre en el navegador o curl:
curl http://127.0.0.1:49219/healthz
curl http://127.0.0.1:49219/state
```
Respuestas esperadas:
- `/healthz` → `{ "ok": true, "version": "0.1.0" }`
- `/state`  → `{ device_id, agent_version, queue_len, cpu_pct, mem_mb, last_event_ts, last_heartbeat_ts }`

## Medición rápida de SLOs (idle)
- Script: `scripts/slo_idle_check.py` (Python 3.8+)
- Mide p95 de CPU (%) y RAM (MB) del proceso consultando `/state` periódicamente.

Ejemplo (120 s, 1 s intervalo):
```
# Medición recomendada en build release
python3 scripts/slo_idle_check.py --duration 120 --interval 1.0 --cpu-threshold 1.0 --mem-threshold 60

# Si prefieres usar build debug: añade --debug-build
```
Notas:
- Por defecto, el script compila y lanza `agent-daemon` y lo cierra al finalizar.
- Usa `--use-running` si ya lo tienes ejecutándose.
- `--json-out result.json` para guardar el resultado.

## Logs
- Rotación diaria a: `.../logs/agent.log` dentro del directorio de datos de la app.
- Nivel configurable con `RUST_LOG` (por ejemplo, `RUST_LOG=info`).
- Rutas típicas:
  - macOS: `~/Library/Application Support/Ripor/RiporAgent/logs/`
  - Linux: `~/.local/share/Ripor/RiporAgent/logs/`
  - Windows: `%APPDATA%\Ripor\RiporAgent\logs\`

## Windows (Fase 0)
- Requisitos: Rust (stable) y PowerShell 5+.
- Ejecutar el agente:
  - `set RUST_LOG=info && cargo run -p agent-daemon`
  - Abre `http://127.0.0.1:49219/` (UI inline) o usa `curl`/PowerShell:
    - `Invoke-RestMethod http://127.0.0.1:49219/healthz`
    - `Invoke-RestMethod http://127.0.0.1:49219/state`
- Smoke test: `powershell -ExecutionPolicy Bypass -File scripts\win_smoke.ps1`
- Helper de configuración/arranque:
  - `powershell -ExecutionPolicy Bypass -File scripts\win_run.ps1 -PanelAddr 127.0.0.1:49219 -IdleActiveThresholdMs 60000 -Run -OpenUI`
  - Flags útiles: `-Debug` (RUST_LOG=debug), `-Force` (sobrescribir .env)
- Notas:
  - El panel escucha solo en loopback; no requiere permisos elevados.
  - `.env` funciona igual (`PANEL_ADDR`, `IDLE_ACTIVE_THRESHOLD_MS`, etc.).
  - Logs: `%APPDATA%\Ripor\RiporAgent\logs\agent.log`.

## Configuración por `.env`
- Crea un archivo `.env` (o copia `.env.example`) en la raíz del repo.
- Variables soportadas clave:
  - `PANEL_ADDR`: dirección de bind del panel. Ej: `127.0.0.1:49219`.
  - `IDLE_ACTIVE_THRESHOLD_MS`: umbral para `ONLINE_ACTIVE/ONLINE_IDLE`.
  - `RIPOR_NO_AUTO_PROMPT`: `1` para desactivar prompts automáticos de permisos en macOS.
  - `HEARTBEAT_URL`, `EVENTS_URL`: endpoints opcionales de backend.
- El agente carga `.env` al iniciar.

Errores comunes al iniciar
- `PANEL_ADDR inválido`: el agente lo registrará en logs y terminará. Corrige el formato `host:puerto` en `.env`.
- `No se pudo abrir el puerto` (address in use): el agente lo registrará y terminará. Cierra el proceso que ocupa el puerto o ajusta `PANEL_ADDR`.

## Heartbeat y envío de eventos (Fase 1)
- Heartbeat local: si no hay eventos por 60 s, el agente registra un heartbeat y actualiza `last_heartbeat_ts` en `/state`.
- Variables de entorno opcionales para backend:
  - `HEARTBEAT_URL=https://tu-backend/v1/agents/heartbeat`
  - `EVENTS_URL=https://tu-backend/v1/agents/events` (activa el sender en background)

Comprobación local del heartbeat:
```
RUST_LOG=info cargo run -p agent-daemon
# espera ~60 s sin actividad y consulta:
curl http://127.0.0.1:49219/state
```

## Permisos en macOS (Transparencia/Captura)
- Qué requiere:
  - Accessibility: para capturar información de la app activa de forma fiable.
  - Screen Recording: para obtener títulos de ventana vía CoreGraphics en algunas apps/OS.
- Endpoints útiles:
  - `GET /permissions` → estado de permisos (macOS): `{ accessibility_ok, screen_recording_ok }`.
  - `GET /permissions/prompt` → intenta mostrar los diálogos del sistema (pueden mostrarse solo una vez; si no aparecen, abre Configuración del Sistema manualmente).
  - `GET /permissions/open/accessibility` → abre el panel de Accesibilidad.
  - `GET /permissions/open/screen` → abre el panel de Screen Recording.
- Dónde habilitar manualmente:
  - System Settings → Privacy & Security → Accessibility → permite el binario del agente.
  - System Settings → Privacy & Security → Screen Recording → permite el binario del agente.
- Efecto en captura:
  - Sin Screen Recording, `window_title` puede venir vacío.
  - Sin Accessibility, la fiabilidad de la app foreground puede variar según la app/OS.
- Ruta del binario a autorizar:
  - El panel (`/` o `/ui`) muestra la ruta exacta de `agent-daemon` que debe tener permisos (copiable). Recomendado usar el binario dentro del bundle `.app` para identidad TCC estable.
- Reset TCC (si algo queda atascado):
  - `tccutil reset ScreenCapture com.ripor.Ripor`
  - `tccutil reset Accessibility com.ripor.Ripor`
  - Abre la app de nuevo y vuelve a conceder permisos.

## Estructura del repositorio
- `crates/agent-core/`
  - `paths.rs`: rutas y archivos (`queue.sqlite`, `agent_state.json`, `key.bin`).
  - `state.rs`: estado del agente y `deviceId` persistente.
  - `crypto.rs`: cifrado AES‑GCM + compresión Zstd.
  - `queue.rs`: cola de eventos cifrados (SQLite/WAL).
  - `metrics.rs`: muestreador periódico de CPU/Mem.
- `crates/agent-daemon/`
  - `main.rs`: servidor HTTP local (`/healthz`, `/state`) y wiring básico.
- `plan.md`: plan por fases, tareas y criterios de aceptación.

## Datos locales y rutas
Se usan directorios de aplicación por SO (según `directories::ProjectDirs`). Archivos principales:
- `queue.sqlite`: cola cifrada de eventos.
- `agent_state.json`: `deviceId`, versión y timestamps.
- `key.bin`: clave simétrica (32 bytes) para cifrado de cola.

## Próximos pasos (alto nivel)
- Completar logs rotativos y ajustes de consumo (SLOs Fase 0).
- Fase 1: captura foreground/título/input idle y heartbeats.
- Preparar `agent-cli` y panel UI ampliado para transparencia.

## Notas de desarrollo
- Logging: controlar nivel con `RUST_LOG` (por ejemplo, `RUST_LOG=info`).
- Seguridad: el panel solo escucha en `127.0.0.1`. No expone CORS.
- Cola: la AAD del cifrado se liga a `deviceId` para endurecer el formato.

---
Este README cubre el arranque de la Base 0. Ajustes y módulos siguientes se documentarán al avanzar las fases.
## macOS — Empaquetado .app + Login Item + Firma/Notarización
- Estructura final (un solo bundle):
  - `Ripor.app`
    - `Contents/MacOS/RiporUI` (UI de barra)
    - `Contents/Resources/bin/agent-daemon` (agente)
    - `Contents/Library/LoginItems/RiporHelper.app` (helper de arranque)
    - `Contents/Resources/iconTemplate.png` (opcional, icono template)
- Empaquetado:
  - `bash scripts/macos_pack.sh` → crea `dist/Ripor.app`.
- Login al iniciar sesión:
  - Próximo paso: Toggle real vía `SMAppService` (LoginItem moderno). Fallback actual: LaunchAgent en `~/Library/LaunchAgents`.
- Firma (ejemplo):
  - Requiere certificado “Developer ID Application” y habilitar Runtime: `--options runtime`.
  - Orden sugerido: firmar `agent-daemon`, luego `RiporHelper.app`, luego `RiporUI`, y por último `Ripor.app`.
  - Entitlements: incluir `com.apple.security.network.client` y mantener sandbox desactivado.
- Notarización (notarytool):
  - `ditto -c -k --keepParent Ripor.app Ripor.zip`
  - `xcrun notarytool submit Ripor.zip --keychain-profile NotaryProfile --wait`
  - `xcrun stapler staple Ripor.app`
  - Verificar: `spctl --assess --type execute -v Ripor.app`
- Iconos:
  - Coloca `assets/icons/macos/iconTemplate.png` (monocromo, transparente). El packer lo copia al bundle.
