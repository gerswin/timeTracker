# Plan del Proyecto — Ripor: Agente Rust Multiplataforma

Documento vivo. Estado corregido contra el código real el **2026-07-13** (auditoría file:line por fase). Producto: **Ripor** (bundle `dist/Ripor.app`, binarios `agent-daemon`, `RiporUI`, `RiporHelper`).

## Leyenda de estado
- [ ] Pendiente
- [x] Completado (verificado en código, con evidencia)
- (parcial) Existe parte; la nota dice qué falta

## Estado global (auditoría 2026-07-13)
- Último commit: 2025-09-18 — proyecto pausado ~10 meses.
- **Tests: 0 en todo el workspace** (3960 líneas .rs, ningún `#[test]`). Viola la regla de calidad del repo.
- **Build roto**: `cargo check --workspace` falla en macOS (`winreg` sin cfg gate en agent-ui-windows). Excluyéndolo compila con 88 warnings (100 clippy).
- Plan anterior subreportaba Fase 5 (~60-70% hecha) y sobrereportaba items de cola/bootstrap; packaging completo sin trackear.
- **Decisiones abiertas resueltas el 2026-07-13** (D1-D6): ver `docs/superpowers/specs/2026-07-13-plan-decisions-design.md`.

## SLOs (métricas objetivo)
- [ ] CPU p95 ≤ 1% (medido solo macOS idle: p95≈0.13% ✓, `slo_mac.json`; falta Windows y bajo carga; Linux → v2)
- [ ] RAM p95 ≤ 60 MB (medido solo macOS idle: p95≈16 MB ✓)
- [ ] Pérdida de eventos < 0.1% (en riesgo: sin GC de cola ni límite de reintentos — ver P0)
- [ ] Aplicación de política ≤ 10 s (implementado POLICY_POLL_SECS=10 + fix retry 401; falta verificación E2E con backend real)
- [ ] MTTR por crash ≤ 5 s (sin watchdog; Fase 6 al 0%)

---

## P0 — Arreglos inmediatos (bloquean build/pruebas; antes de cualquier feature)
- [ ] `agent-ui-windows/Cargo.toml:10`: mover `winreg` a `[target.'cfg(windows)'.dependencies]` — hoy rompe `cargo check --workspace` en macOS/Linux
- [ ] `agent-login-macos`: agregarlo a workspace members (`Cargo.toml` raíz) y corregir `[bin]` → `[[bin]]` — hoy no compila, rompe `macos_pack.sh` y deja `dist/Ripor.app` sin RiporHelper.app (cadena login-item no testeable)
- [x] UI inline en `/`: `<script>` roto (SyntaxError, `main.rs:444-501`). **Decidido (D5): eliminar la UI inline** — `/` y `/ui` redirigen a `/panel`, panel embebido en el binario, botón "Refrescar política" pasa a `/panel` (hecho: `/` y `/ui` redirigen, panel embebido; `/panel` está embebido en el binario con override `PANEL_DIR`)
- [ ] Cola: incrementar `attempts` en reintentos + límite de reintentos + GC por tamaño/edad (`queue.rs` — hoy `attempts` es schema muerto; batch fallido reintenta para siempre, cola crece sin límite)
- [ ] Limpieza: borrar `src/main.rs` raíz ("Hello, world!" muerto), `.gitignore` para `dist/` y `.DS_Store` (hoy hay 4.4 MB de binarios sin firma commiteados)
- [ ] Reducir warnings (88 compilador / 100 clippy): dead code (`HeartbeatPayload`, `Throttled`, `screen_recording_allowed`), imports sin uso, statics mutables

---

## Seguridad — hallazgos de auditoría (pre-requisitos de los DoD de Fase 2 y de la sección Seguridad)
- [x] **Clave AES en `key.bin` plaintext world-readable** junto a `queue.sqlite` (`crypto.rs:25`). **Decidido (D6): Keychain (macOS) / DPAPI (Windows) ahora**, fallback archivo 0600, migración automática de `key.bin` (keystore vía keyring + fallback 0600 + migración key.bin)
- [x] **Títulos de ventana en plaintext en logs rotativos** a nivel info (`capture.rs:179`). **Decidido (D3): redactar título en logs salvo `RIPOR_DEBUG=1`** (redacción salvo RIPOR_DEBUG=1)
- [x] **`/debug/drops` expone títulos sensibles excluidos** por HTTP local (`main.rs:575-577`, `policy.rs:80-85`). **Decidido (D3): título completo solo con `RIPOR_DEBUG=1`**; por defecto DropLog guarda app + razón + hash corto (DropLog redactado por defecto)
- [ ] **Forzar `https://` en `API_BASE_URL`** (hoy se consume raw, `net.rs:34,105,267`); políticas sin firma → canal de policy (killSwitch, titleCapture, excludes) spoofeable por MITM
- [ ] **Guard de loopback en `PANEL_ADDR`** (hoy rebind a cualquier interfaz sin validación) + bloqueo CORS/Origin explícito
- [ ] Contabilizar drops de `excludeExePaths` con su propia razón (hoy se cuentan como `excludedPattern`, `capture.rs`, distorsiona telemetría)

---

## Testing — deuda transversal (antes de nuevas features)
- [ ] Harness de tests unitarios en workspace (hoy 0 tests). Unidades puras baratas primero:
  - [ ] `FocusAgg` (consolidación, ráfagas, switching rápido — cubre DoD Fase 3)
  - [ ] `Throttle::permit` (token bucket, force_emit, refill — cubre DoD Fase 4)
  - [ ] `drop_reason` (excludeApps/Patterns/ExePaths, killSwitch, pause)
  - [ ] `crypto` roundtrip (zstd+AES-GCM, AAD device_id)
  - [ ] Cola: enqueue/peek/delete/attempts/GC
- [ ] E2E mínimo: `smoke.sh` ya existe (build + /healthz + /state + transición ACTIVE/IDLE) — integrarlo como gate; variante Windows `win_smoke.ps1` sin ejercitar

---

## Alcance v1 — decidido 2026-07-13 (D1)
**v1 = macOS + Windows. Linux diferido a v2** (estaba al 0% real; ver sección "v2 — Linux" al final).

---

## Fase 0 — Fundaciones
Objetivo: base multiplataforma, cola cifrada, panel local mínimo, estado y métricas.

Tareas
- [x] Workspace Rust (`agent-core`, `agent-daemon`; luego `agent-cli`, `agent-ui-macos`, `agent-ui-windows`; `agent-login-macos` fuera de members — ver P0)
- [x] Dependencias: `tokio`, `axum`, `rusqlite`, `serde`, `zstd`, `aes-gcm`, `tracing`, `sysinfo`
- [x] `deviceId` estable y persistente (`state.rs:16-36`)
- [ ] Cola `queue.sqlite` (parcial: WAL ✓, índice `created_at` ✓; **falta índice `attempts` y toda la mecánica de reintentos/GC** — ver P0)
- [x] Compresión `zstd` + cifrado AES-256-GCM antes de persistir (`crypto.rs:29-44`; clave insegura — ver Seguridad)
- [x] Panel local `127.0.0.1:49219` con `/healthz`, `/state` (loopback es default, no forzado — ver Seguridad)
- [x] Logs rotativos diarios + `RUST_LOG` (sin cap de tamaño ni prune de archivos viejos)
- [x] Telemetría local: `cpuPct`, `memMb` (`metrics.rs:28-60`)
- [x] Script `scripts/slo_idle_check.py` (+ variante `.sh`)

DoD
- [ ] Arranque estable en macOS/Windows sin elevación (macOS verificado; Win sin verificar; Linux → v2)
- [x] Panel muestra versión y estado
- [x] p95 CPU idle < 1% y RAM < 60 MB (macOS: 0.13% / 16 MB; falta Win/Linux)

---

## Fase 1 — Captura + Heartbeat
Objetivo: app/título foreground, `inputIdleMs`, heartbeats 60 s.

Tareas
- [x] Windows: foreground + título + `GetLastInputInfo` (`capture.rs:521-699`; sin compilar/verificar en Windows real)
- [x] macOS: AX focused app → CGWindowList → AXUIElement → NSWorkspace + idle CGEventSource (`capture.rs:302-347`)
- Linux: X11/Wayland + idle → **diferido a v2** (D1)
- [x] Estado `ONLINE_ACTIVE/ONLINE_IDLE` (`main.rs:713-724`)
- [x] Heartbeat 60 s, canal independiente de la cola (`net.rs:25-93`)
- [x] **Decidido (D2): heartbeat SIEMPRE** — quitar supresión cuando hay eventos (`net.rs:37-40`) y añadir `device_id` al body (struct muerto `net.rs:16-23`) (implementado; body serializa HeartbeatPayload)
- [x] Batch sender con backoff exponencial cap 60 s, borra solo en 2xx (`net.rs:103-241`). **Activado por `API_BASE_URL`** (no `EVENTS_URL` — corregir README que documenta vars stale)
- [x] Endpoint `/queue` con preview descifrado
- [x] macOS permisos: `/permissions`, `/permissions/prompt` (+ `/permissions/open/accessibility`, `/permissions/open/screen` — antes sin trackear)

DoD
- [ ] 30 min sin eventos → 30 heartbeats entregados (mecanismo lo soporta; nunca verificado con backend real — hacer soak test)
- [x] `/state` muestra `lastHeartbeatTs`, `queueLen`, preview
- [x] Persistencia offline y drenaje al volver la red (verificado por código; sin test)

---

## Fase 2 — Políticas + Exclusiones
Objetivo: bootstrap y políticas remotas con ETag; filtro antes de persistir; kill switch/pause.

Tareas
- [ ] Bootstrap (parcial): shape request/response ✓, re-bootstrap one-shot en 401 para heartbeat/ingest/policy ✓, cache `policy.json`+`policy_meta.json` ✓; **falta persistencia segura** — secretos en `agent_secrets.json` chmod 600, no Keychain/DPAPI
- [x] `GET /v1/policy/{user_email}` con `If-None-Match`/ETag (`net.rs:336-410`)
- [x] Hot-apply local ≤ 10 s (`capture.rs:112-113` relee policy por tick; `/policy/apply` y `/policy/refresh`). Nota: poll remoto automático es cada **300 s** — el DoD de ≤10 s remoto requiere bajar el intervalo o push
- [x] `excludeApps[]`, `excludePatterns[]` antes de encolar (`capture.rs:209-219`)
- [ ] `dropped_reason` en evento excluido (parcial: el evento nunca se persiste — la razón vive en DropLog memoria + `/debug/drops`; decidir si el item sigue aplicando tal como está redactado)
- [x] Telemetría `dropped_events` total y por razón (excludeExePaths mal atribuido — ver Seguridad)
- [x] `killSwitch`/`pauseCapture` respetados; heartbeats siguen
- [x] CLI `agent policy show|pull` (+ extras `open|apply|edit|refresh` — antes sin trackear, `agent-cli/src/main.rs:24-48`)
- [x] Panel `/panel`: política efectiva (ETag + JSON) y contadores de descartes
- [ ] `/policy/refresh` endpoint ✓ + CLI ✓; **botón UI roto** (vive en UI inline con SyntaxError — ver P0)
- [x] `excludeExePaths[]` (macOS bundleId, Windows exe path; Linux N/A)
- [x] Forced emit respeta token bucket, salta solo debounce (`capture.rs:288-298`)

DoD
- [ ] Títulos sensibles nunca persisten ni salen del proceso (**violado hoy** por logs plaintext y `/debug/drops` — ver Seguridad)
- [x] Panel muestra política efectiva y ETag
- [ ] Cambios de política ≤ 10 s — (implementado POLICY_POLL_SECS=10 + fix retry 401; falta verificación E2E con backend real)
- [x] Bootstrap completado, `agentToken` persistido/usable

---

## Fase 3 — Categorías + Focus
Objetivo: categorías embebidas y agregación de foco.

Tareas
- [ ] `appCategories.sqlite` embebida (0%)
- [ ] Sync diferencial `GET /v1/categories` (ETag) (0%)
- [ ] Campo `category` en eventos (hoy hardcoded `""` en `net.rs:149,172` — ni siquiera `Uncategorized`)
- [x] Agregador de focus app+título > `focusMinMinutes` (`capture.rs:32-79`). Limitación conocida: bloque solo se finaliza al cambiar app/título — un focus largo en curso es invisible para `/focus/blocks` hasta el switch
- [x] `focusMinMinutes` (default 5)
- [x] Persistencia `focus_blocks` + prune (por conteo, keep 1000 — no por edad)
- [x] `/focus/blocks?limit=N[&min_minutes=M]`
- [x] `/focus/aggregate?days=N`
- [x] UI tabla de bloques recientes
- [x] Export CSV `/focus/aggregate.csv?days=N` + link en panel (commit `dc36935`). Bug menor: columna `dur_hhmm` calcula mm:ss, no hh:mm (`main.rs:667-669`)

DoD
- [ ] Bloques sin huecos en ráfagas/switching rápido (sin test — ver Testing)
- [ ] `category` presente en eventos
- [x] Panel lista bloques + sumas por día/app
- [x] Export CSV disponible

---

## Fase 4 — Throttling + Actividad real
Objetivo: muestreo de títulos y `ONLINE_PASSIVE` para videollamadas.

Tareas
- [ ] Muestreo 1–2 Hz + debounce (parcial: loop fijo 1 Hz `capture.rs:194` + min-interval 500 ms como espaciado de emisión; `titleSampleHz` solo ajusta el min-interval, **no** la frecuencia real de muestreo; no hay debounce de estabilidad real)
- [ ] Límite ≤ 10 títulos/min/**app** (parcial: token bucket 10/min existe `capture.rs:266-299` pero es **global**, no por app)
- [ ] Heurística media (Teams/Zoom/Meet) → `mediaHint` (0%: campo hardcoded `""`)
- [ ] Solo media + sin input > M min → `ONLINE_PASSIVE` (0%: estado no existe)
- [x] Exponer `titleSampleHz`, `titleBurstPerMinute` en política (cableado end-to-end `policy.rs:24-26` → `capture.rs:277-279`; antes doble-contabilizado en Fase 2)

DoD
- [ ] Títulos a 10 Hz → ≤ 2 Hz persistidos (mecanismo lo garantiza por construcción; sin test)
- [ ] Llamada 60 min sin input → `ONLINE_PASSIVE` estable

---

## Fase 5 — UI de Transparencia
Objetivo: indicadores visibles, panel completo, pausas temporizadas.
Estado real: **~60-70% en macOS/Windows** (el plan anterior decía 0%).

Tareas
- [ ] Windows (parcial): tray + menú (Ver panel / Pausar 15/60 / Reanudar / autorun HKCU / Salir) ✓ (`agent-ui-windows/src/main.rs:22-90`); **falta Toast WinRT**
- [ ] macOS (parcial): NSStatusItem + menú completo (política, permisos AX/Screen con estado, pausas, login item toggle, Salir) ✓ (`agent-ui-macos/src/main.rs:45-138`); **falta NSAlert en cambios de política**
- Linux: AppIndicator + notify-rust → **diferido a v2** (D1)
- [ ] Panel local completo (parcial: `/panel` SPA con refresh 2 s ✓; falta "últimos envíos", enforcement loopback y bloqueo CORS — ver Seguridad. **D5 implementado**: la UI inline ya no existe; `/panel` embebido en el binario es la única UI, botón refrescar política vive ahí)
- [ ] CLI (parcial): `agent policy open` abre panel ✓ (renombrar o alias a `privacy open`); **falta `agent pause --minutes N`** (el daemon ya soporta `/pause?minutes=N` y `/pause/clear` — solo falta el subcomando)
- [x] Endpoints de pausa temporizada `/pause`, `/pause/clear` + `paused_until_ms` en `/state` (antes sin trackear)
- [x] Login item macOS: SMAppService (shim ObjC `macos_loginitem.m`) + fallback LaunchAgent + `--print-login-state` (antes sin trackear)

DoD
- [ ] Tray visible siempre en macOS/Windows (implementado, autostart incluido; "siempre" sin verificar en sesión real; Linux → v2)
- [x] Panel abre vía CLI y refleja estado near-real-time (refresh 2 s)

---

## Fase 6 — OTA + Anti-manipulación
Estado real: 0% de código. Único adyacente: infra de firma macOS (scripts) — ver Packaging.

Tareas
- [ ] Checksum SHA-256 del binario + manifiesto firmado (sha2 ya es dep, pero solo se usa para HMAC de requests)
- [ ] Windows: servicio updater (elevación solo en apply) + delta patches
- [ ] macOS: Sparkle 2 + Ed25519 (canales estable/beta)
- Linux: updater + APT/YUM + verificación de firma → **diferido a v2** (D1)
- [ ] Watchdog (servicio SO) + `sentinel` para hangs (nota: `agent-login-macos` NO es esto — solo lanza el daemon al login y termina)
- [ ] Detección debug (ptrace/IsDebuggerPresent) → `tamper=DEBUG_DETECTED` + modo solo-heartbeat
- [ ] Rollback atómico + telemetría de update

DoD
- [ ] Matar proceso → watchdog revive < 5 s
- [ ] Update firmado aplicado y rollback probado
- [ ] Evento tamper registrado y degradación a modo protegido

---

## Fase 7 — Endurecimiento + Métricas

Tareas
- [ ] Métricas (parcial: `cpu_pct`, `mem_mb`, `queue_len`, dropped por razón acumulado ✓; faltan `events_sent/s`, `flush_latency_ms`, `tamper_flags`, `heartbeat_ok`, y tasas en vez de acumulados)
- [ ] GC de cola por tamaño/edad; límites de backoff y reintentos (parcial: backoff exponencial cap 60 s ✓; GC y límite de reintentos 0% — ver P0)
- [ ] `agent diag --export /tmp/agent_diag.zip` (0%: CLI solo tiene `policy`)
- [ ] Verificación de SLOs y pruebas E2E (parcial: scripts existen; nada automatizado ni ejecutado salvo idle macOS)

DoD
- [ ] SLOs marcados como cumplidos en este documento
- [ ] Export de diagnósticos completo

---

## Packaging & Distribution (nuevo — antes sin trackear)
- [x] `scripts/macos_pack.sh`: ensambla `dist/Ripor.app` (RiporUI + agent-daemon + estructura LoginItems/RiporHelper)
- [ ] Pack completo bloqueado por `agent-login-macos` roto (ver P0) — el bundle actual carece de RiporHelper.app
- [ ] Firma real: `scripts/macos_sign.sh` usa placeholder `Developer ID Application: YOUR NAME (TEAMID)`; el bundle commiteado tiene firma **adhoc**. Configurar cert real + hardened runtime
- [ ] Notarización: `scripts/macos_notarize.sh` (notarytool + stapler + spctl) depende de keychain profile `NotaryProfile` no configurado; nunca ejercitado
- [ ] Icono macOS: `assets/icons/macos/iconTemplate.png` referenciado por pack.sh no existe en el repo (Windows `.ico` sí, embebido)
- [ ] Política de `dist/`: dejar de commitear binarios (gitignore — ver P0); publicar por releases/CI
- [ ] Windows: sin instalador/packaging (solo `win_run.ps1` de desarrollo)
- [x] Entitlements macOS (`assets/macos/*.plist`, mínimos: sin sandbox, network client)

---

## Backend mínimo (coordinación) — lado cliente verificado; el servidor no vive en este repo
- [x] `POST /v1/events:ingest` (Agent-Token + X-Body-HMAC) — cliente completo (`net.rs:183-249`)
- [x] `POST /v1/agents/heartbeat` — cliente completo (suprimido si hubo evento <60 s; omite `device_id`)
- [x] `POST /v1/agents/bootstrap` (sin auth) — cliente completo
- [x] `GET /v1/policy/{user_email}` (ETag) — cliente completo (duplicaba Fase 2; el soporte ETag del servidor no es verificable desde este repo)
- [ ] `GET /v1/categories` (ETag) — 0%
- [ ] Autenticación por tenant + `deviceId` — no verificable desde este repo (cliente envía org_id/user_email/device_id + HMAC)

---

## Inventario real de superficie (antes sin trackear — mantener sincronizado)
- Endpoints daemon: `/healthz`, `/state`, `/queue`, `/panel` (SPA estática), `/` y `/ui` (inline, rota — P0), `/pause`, `/pause/clear`, `/policy/apply`, `/policy/refresh`, `/focus/blocks`, `/focus/aggregate[.csv]`, `/permissions[/prompt|/open/*]`, `/debug/drops`, `/debug/sample|windows|window|frontmost`
- CLI: `agent policy show|pull|open|apply|edit|refresh` (faltan: `pause`, `privacy open`, `diag`)
- Config: `.env` (`API_BASE_URL`, `PANEL_ADDR`, `IDLE_ACTIVE_THRESHOLD_MS`, `RIPOR_NO_AUTO_PROMPT`, `RIPOR_DEBUG_INGEST`, `POLICY_POLL_SECS`, `RIPOR_DEBUG`…) — README documenta vars stale `EVENTS_URL`/`HEARTBEAT_URL`, corregir
- Scripts: `smoke.sh`, `slo_idle_check.py|.sh`, `win_run.ps1`, `win_smoke.ps1`, `macos_pack|sign|notarize.sh`

---

## Seguridad y privacidad (objetivo de diseño; hallazgos concretos arriba)
- [ ] Cola cifrada en reposo con clave en Keychain/DPAPI/libsecret (hoy: cifrado ✓, clave plaintext ✗)
- [ ] TLS forzado a backend; pinning opcional; políticas firmadas (hoy: rustls compilado ✓, https no forzado ✗, firma ✗)
- [x] Filtro de exclusiones antes de persistir (orden correcto en `capture.rs:114-126`; fugas por logs y `/debug/drops` — ver Seguridad)

---

## Pruebas clave (checklist E2E)
- [ ] Transparencia: tray visible; panel solo loopback; sin elevación
- [ ] Exclusiones: títulos sensibles nunca persisten ni salen del proceso
- [ ] Focus: ráfagas sin huecos; switching rápido estable
- [ ] Actividad real: llamada 60 min sin input → `ONLINE_PASSIVE`
- [ ] Throttling: 10 Hz → ≤ 2 Hz emitidos
- [ ] Health: 30 min sin eventos → 30 heartbeats (soak con backend real)
- [ ] Tamper: matar proceso → revive < 5 s; debug → modo protegido
- [ ] OTA: update firmado + rollback atómico

---

## Riesgos y mitigaciones
- Wayland/DE heterogéneos: **diferido a v2 con Linux** (D1)
- [ ] Servicios de SO: macOS login-item ✓ (roto por P0); systemd user y servicio Windows 0%
- [ ] Consumo: zstd nivel 3 ✓, throttle ✓; falta muestreo adaptativo y sleep extra en idle
- [ ] OTA corrupto: N/A hasta que exista OTA (solo campo `updateChannel` sin uso en policy)

---

## Próximos pasos sugeridos (orden)
1. Implementar decisiones D5 → D2 → D4 → D3 → D6 (spec `docs/superpowers/specs/2026-07-13-plan-decisions-design.md`)
2. P0 restante (winreg gate, agent-login-macos, cola con reintentos acotados, limpieza)
3. Harness de tests + primeras 5 unidades puras
4. Cerrar Fase 5 macOS/Windows (Toast, NSAlert, `agent pause`) — es lo más cerca de terminarse
5. Fase 4 restante (mediaHint, ONLINE_PASSIVE, bucket por app)

---

## v2 — Linux (diferido, decidido 2026-07-13, D1)
- [ ] Captura X11/Wayland (preferir Wayland; fallback X11) + idle
- [ ] Tray AppIndicator (libappindicator/ayatana) + notificaciones (notify-rust)
- [ ] Packaging deb/rpm + updater con verificación de firma
- [ ] Clave en libsecret
- [ ] Riesgo Wayland/DE heterogéneos: detectar capacidades, documentar límites
