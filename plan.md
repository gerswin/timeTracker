# Plan del Proyecto — Agente Rust Multiplataforma

Documento vivo para marcar hitos, tareas y resultados del agente (Windows 10/11, macOS 12+, Ubuntu 20.04+). Marca cada casilla al completar.

## Leyenda de estado
- [ ] Pendiente
- [x] Completado
- (en progreso) Añade esta nota al ítem correspondiente

## SLOs (métricas objetivo)
- [ ] CPU p95 ≤ 1%
- [ ] RAM p95 ≤ 60 MB
- [ ] Pérdida de eventos < 0.1%
- [ ] Aplicación de política ≤ 10 s
- [ ] MTTR por crash ≤ 5 s

## Hitos globales (resumen por fase)
- [x] Fase 0 — Fundaciones (código listo; pendiente validar SLOs en Win/Linux)
- [ ] Fase 1 — Captura + Heartbeat (en progreso)
- [ ] Fase 1 — Captura + Heartbeat
- [ ] Fase 2 — Políticas + Exclusiones
- [ ] Fase 3 — Categorías + Focus
- [ ] Fase 4 — Throttling + Actividad real
- [ ] Fase 5 — UI Transparencia
- [ ] Fase 6 — OTA + Anti‑tamper
- [ ] Fase 7 — Endurecimiento + Métricas

---

## Fase 0 — Fundaciones (1–2 semanas)
Objetivo: base multiplataforma, cola cifrada, panel local mínimo, estado del agente y métricas básicas.

Tareas
- [x] Crear workspace Rust (crates iniciales: `agent-core`, `agent-daemon`)
- [x] Integrar dependencias: `tokio`, `axum`, `rusqlite`, `serde`, `zstd`, `aes-gcm`, `tracing`, `sysinfo`
- [x] `deviceId` estable y persistente
- [x] Cola `queue.sqlite` cifrada (WAL, índices por `created_at`, `attempts`)
- [x] Compresión `zstd` + cifrado antes de persistir
- [x] Panel local en `http://127.0.0.1:49219` (solo loopback) con `/healthz`, `/state`
- [x] Logs rotativos + nivel trazas configurable
- [x] Telemetría local básica: `cpuPct`, `memMb`
- [x] Script `scripts/slo_idle_check.py` para medir p95 CPU/RAM (idle)

DoD (Definition of Done)
- [ ] Arranque estable en Win/macOS/Linux (sin elevación)
- [x] Panel muestra versión y estado del agente
- [x] p95 CPU en idle < 1% y RAM < 60 MB (medido en macOS: CPU p95≈0.13%, RAM p95≈16 MB)

---

## Fase 1 — Captura + Heartbeat (1–2 semanas)
Objetivo: capturar app/título foreground, `inputIdleMs`; heartbeats cada 60 s.

Tareas
- [x] Windows: foreground window + título + `GetLastInputInfo`
- [x] macOS: app foreground (NSWorkspace) + título (CGWindowList; fallback AXUIElement) + `inputIdleMs`
- [ ] Linux: X11/Wayland (preferir Wayland si disponible; fallback X11) + idle
- [x] Estado `ONLINE_ACTIVE/ONLINE_IDLE` derivado de `inputIdleMs`
- [x] Heartbeat cada 60 s (canal independiente de la cola)
- [x] Batch sender con backoff (opcional, activado por `EVENTS_URL`)
- [x] Endpoint panel `/queue` con preview de cola
- [x] Endpoints macOS permisos: `/permissions`, `/permissions/prompt` (abre System Settings para Screen Recording)

DoD
- [ ] 30 min sin eventos → 30 heartbeats entregados
- [x] Panel (`/state`) muestra `lastHeartbeatTs`, `queueLen`, y preview de cola
- [x] Eventos básicos persisten offline y se vacían al volver la red

---

## Fase 2 — Políticas + Exclusiones (1–2 semanas)
Objetivo: onboarding (login/bootstrap) y políticas remotas con ETag; filtro antes de persistir; drop rate por regla; kill switch/pause.

Tareas
- [x] Bootstrap/login del agente:
  - `POST /v1/agents/bootstrap`
    - Request JSON: `{ "org_id": "string", "user_email": "string", "mac_address": "string", "agent_version": "string" }`
    - Response JSON: `{ "agentToken": string, "serverSalt": string, "deviceId": string }`
  - Persistir `agentToken` y `serverSalt` (seguro: Keychain/DPAPI cuando aplique) y cachear `policy.json`
  - Reintento automático en 401 (re-bootstrap one‑shot y reenvío)
- [x] `GET /v1/policy/{user_email}` con `If-None-Match` + ETag
- [x] Aplicación en caliente ≤ 10 s (cache local `policy.json` + `policy_meta.json`)
- [x] Reglas: `excludeApps[]`, `excludePatterns[]` (en captura)
- [ ] Marcar evento excluido con `dropped_reason`
- [x] Telemetría: `dropped_events` total y por razón (throttled/excluded/pause/killSwitch)
- [x] `killSwitch` y `pauseCapture` respetados (heartbeats siguen activos)
- [ ] CLI: `agent policy show|pull`
- [x] Panel/UI: mostrar política efectiva (ETag + JSON) y contadores de descartes
- [x] Endpoint `/policy/refresh` y botón/CLI para refrescar policy en caliente
- [x] `excludeExePaths[]` aplicado (macOS bundleId, Windows ruta de ejecutable)
- [x] Ajuste throttling: forced emit respeta token bucket (salta solo debounce)

DoD
- [ ] Títulos sensibles nunca persisten ni salen del proceso
- [x] Panel muestra política efectiva y versión/ETag
- [ ] Cambios de política se reflejan ≤ 10 s
 - [x] Bootstrap completado y `agentToken` persistido/usable

---

## Fase 3 — Categorías + Focus (1–2 semanas)
Objetivo: categorías embebidas y agregación de foco por app+titulo.

Tareas
- [ ] `appCategories.sqlite` embebida (hash exe/bundleId → categoría)
- [ ] Sync diferencial con `GET /v1/categories` (ETag)
- [ ] Campo `category` en cada evento (fallback `Uncategorized`)
- [x] Agregador de focus: consolidar bloques si app+title constantes > `focusMinMinutes`
- [x] Política `focusMinMinutes` (default 5)
- [x] Persistencia de bloques en SQLite (`focus_blocks`) + prune
- [x] Endpoint `/focus/blocks?limit=N[&min_minutes=M]`
- [x] Endpoint `/focus/aggregate?days=N` (sumas por día y app)
- [x] UI tabla de bloques recientes (app/título/inicio/fin/duración)
- [ ] Export CSV de agregados: `/focus/aggregate.csv?days=N`

DoD
- [ ] Bloques de focus sin huecos en ráfagas y switching rápido
- [ ] `category` presente en eventos
- [x] Panel lista últimos bloques de focus + sumas por día/app
- [ ] Export CSV disponible para analítica

---

## Fase 4 — Throttling + Actividad real (1–2 semanas)
Objetivo: muestreo de títulos y detección `ONLINE_PASSIVE` para videollamadas.

Tareas
- [ ] Muestreo 1–2 Hz máx + debounce 300–500 ms
- [ ] Límite sin focus: ≤ 10 títulos/min/app (token bucket)
- [ ] Heurística media (Teams/Zoom/Meet) → `mediaHint`
- [ ] Si solo media y sin input > M min → `ONLINE_PASSIVE`
- [ ] Exponer `titleSampleHz`, `titleBurstPerMinute` en política

DoD
- [ ] Títulos a 10 Hz generan ≤ 2 Hz persistidos
- [ ] Llamadas de 60 min sin input → `ONLINE_PASSIVE` estable

---

## Fase 5 — UI de Transparencia (1–2 semanas)
Objetivo: indicadores visibles y panel completo; pausas temporizadas.

Tareas
- [ ] Windows: tray icon + menú (Ver política / Pausar…) + Toast (WinRT)
- [ ] macOS: `NSStatusItem` + `NSAlert` para cambios de política
- [ ] Linux: AppIndicator (libappindicator/ayatana) + notifs (`notify-rust`)
- [ ] Panel local: política efectiva, versión, estado, últimos envíos (solo loopback, CORS bloqueado)
- [ ] CLI: `agent privacy open`, `agent pause --minutes N`

DoD
- [ ] Tray visible siempre en los 3 SO
- [ ] Panel abre vía CLI y refleja estado en tiempo (near) real

---

## Fase 6 — OTA + Anti‑manipulación (2 semanas)
Objetivo: actualizaciones seguras y mecanismos básicos anti‑tamper.

Tareas
- [ ] Checksum SHA‑256 del binario al inicio y tras OTA (manifiesto firmado)
- [ ] Windows: servicio updater (elevación solo en apply) + delta patches
- [ ] macOS: Sparkle 2 con firmas Ed25519 (canales estable/beta)
- [ ] Linux: updater interno + soporte APT/YUM; verificación de firma antes de swap
- [ ] Watchdog (servicio SO) + proceso `sentinel` para hangs
- [ ] Detección de debug (ptrace/dbg) en release → evento `tamper=DEBUG_DETECTED` y modo solo‑heartbeat
- [ ] Rollback atómico y telemetría de update

DoD
- [ ] Matar proceso → watchdog lo levanta < 5 s (MTTR)
- [ ] Update firmado aplicado y rollback probado
- [ ] Evento de tamper registrado y degradación a modo protegido

---

## Fase 7 — Endurecimiento + Métricas (1 semana)
Objetivo: telemetría completa, GC, límites, backoff, diagnósticos y verificación de SLOs.

Tareas
- [ ] Métricas: `events_sent/s`, `events_dropped/s` por regla, `queue_size`, `flush_latency_ms`, `cpu_pct`, `mem_mb`, `tamper_flags`, `heartbeat_ok`
- [ ] GC de cola por tamaño/edad; límites de backoff y reintentos
- [ ] `agent diag --export /tmp/agent_diag.zip` (DBs, logs, manifiestos)
- [ ] Verificación de SLOs y pruebas clave E2E

DoD
- [ ] SLOs marcados como cumplidos en este documento
- [ ] Export diagnósticos incluye todo lo necesario para soporte

---

## Pruebas clave (checklist)
- [ ] Transparencia: tray visible siempre; panel solo loopback; sin elevación
- [ ] Exclusiones: títulos sensibles nunca persisten ni salen del proceso
- [ ] Focus: unir ráfagas sin crear huecos; switching rápido estable
- [ ] Actividad real: llamada de 60 min sin input → `ONLINE_PASSIVE`
- [ ] Throttling: títulos a 10 Hz → ≤ 2 Hz emitidos
- [ ] Health: 30 min sin eventos → 30 heartbeats entregados
- [ ] Tamper: matar proceso → watchdog revive < 5 s; debug → modo protegido
- [ ] OTA: actualización firmada aplicada; rollback atómico verificado

---

## Backend mínimo (para coordinación)
- [x] `POST /v1/events:ingest` (batch; requiere Agent-Token + X-Body-HMAC)
- [x] `POST /v1/agents/heartbeat` (requiere Agent-Token + X-Body-HMAC)
- [x] `POST /v1/agents/bootstrap` (login del agente; sin auth)
- [ ] `GET /v1/policy/{user_email}` (ETag; requiere Agent-Token)
- [ ] `GET /v1/categories` (ETag)
- [ ] Autenticación por tenant + `deviceId`

---

## Seguridad y privacidad
- [ ] Cola cifrada en reposo (clave por dispositivo; uso de Keychain/DPAPI/libsecret cuando aplique)
- [ ] TLS a backend; pinning opcional; políticas firmadas
- [ ] Filtro de exclusiones antes de persistir (defensa de datos sensibles)

---

## Riesgos y mitigaciones
- [ ] Wayland/DE heterogéneos: detectar capacidades y fallback a X11; documentar límites
- [ ] Servicios de SO/permiso: usar LaunchAgent/systemd user; elevación mínima en Windows solo para OTA apply
- [ ] Consumo: muestreo adaptativo, sleeps cuando idle, `zstd` nivel bajo
- [ ] OTA corrupto: verificación doble + rollback atómico (slot A/B opcional)

---

## Plantilla de registro de evidencia
Para cada hito, añade bajo la tarea:
- Responsable: @nombre
- Fecha fin: YYYY‑MM‑DD
- Evidencia: commit/tag, build ID, capturas o logs relevantes

---

## Próximos pasos inmediatos
- [ ] Confirmar crates permitidos y APIs por SO
- [ ] Acordar formato final de endpoints y autenticación
- [ ] Iniciar Fase 0 (esqueleto, panel local, cola cifrada)
