# Decisiones abiertas del plan — Diseño

Fecha: 2026-07-13
Contexto: auditoría plan-vs-código del 2026-07-13 (plan.md commit `HEAD`) dejó 6 decisiones abiertas. Este spec las resuelve y define las consecuencias en código y en plan.md. El resto del plan no cambia.

## D1 — Alcance Linux: diferido a v2

**Decisión**: v1 = macOS + Windows. Linux completo (captura X11/Wayland, AppIndicator, packaging deb/rpm, updater) se difiere a v2.

**Racional**: Linux está al 0% real (captura stub en `capture.rs:356-359`, sin tray, sin packaging). Wayland/DE heterogéneos es el riesgo técnico más alto del proyecto y no hay usuario Linux objetivo definido. Diferirlo achica el camino crítico de Fases 1/5/6 ~un tercio.

**Cambios**:
- Código: ninguno. Los stubs Linux quedan (compilan y devuelven vacío).
- plan.md: items Linux de Fases 1, 5 y 6 y el riesgo Wayland se mueven a sección nueva "v2 — Linux (diferido)".

**Criterio de aceptación**: ningún item Linux en el camino crítico de v1 en plan.md.

## D2 — Heartbeat: siempre, con device_id

**Decisión**: heartbeat cada 60 s sin supresión, con `device_id` en el body.

**Racional**: hoy se suprime si hubo evento en los últimos 60 s (`net.rs:37-40`) y el body omite `device_id` (struct muerto `HeartbeatPayload`, `net.rs:16-23`). "Siempre" es más simple de razonar y monitorear en backend; costo trivial (1 request/min). El DoD "30 min → 30 heartbeats" pasa a valer siempre.

**Cambios**:
- `net.rs`: eliminar la condición de supresión; incluir `device_id` en el body reusando el struct `HeartbeatPayload` existente (serializarlo en vez del JSON ad-hoc actual).
- plan.md: quitar la nota de supresión de Fase 1; DoD sin la calificación "solo idle".

**Criterio de aceptación**: con agente activo emitiendo eventos, el heartbeat sigue llegando cada 60 s; body incluye `device_id`. Test unitario del payload; verificación manual o soak contra backend.

## D3 — Títulos sensibles: gate por RIPOR_DEBUG=1 (DropLog y logs)

**Decisión**: el título completo de eventos excluidos solo se retiene/expone con `RIPOR_DEBUG=1`. Sin el env: DropLog guarda app + razón + hash corto del título (redactado), y `/debug/drops` sirve eso. La misma regla aplica a los logs de captura: `capture.rs:179` hoy escribe títulos plaintext a nivel info — sin `RIPOR_DEBUG=1` el título va redactado también en logs.

**Racional**: `/debug/drops` y los logs violan hoy el DoD "títulos sensibles nunca salen del proceso". El gate conserva la utilidad de debug local explícito sin fugar datos en operación normal.

**Cambios**:
- `policy.rs` (DropLog): almacenar título redactado salvo `RIPOR_DEBUG=1`.
- `capture.rs`: redactar título en trazas info salvo `RIPOR_DEBUG=1`.
- plan.md: hallazgos de seguridad 2 y 3 pasan a referenciar esta decisión.

**Criterio de aceptación**: sin `RIPOR_DEBUG`, ningún título de evento excluido aparece en `/debug/drops` ni en archivos de log. Test unitario de la redacción.

## D4 — Policy poll: 300 s → 10 s

**Decisión**: intervalo de poll remoto configurable `POLICY_POLL_SECS`, default 10 s.

**Racional**: el DoD original "cambios ≤ 10 s" se mantiene tal cual. ETag ya está implementado, los 304 son baratos. Carga: 8 640 requests/día/agente, aceptable.

**Cambios**:
- `net.rs`: intervalo 300 → env `POLICY_POLL_SECS` (default 10).
- `net.rs:377`: fix del retry post-401 que descarta la respuesta (`let _ = r2.send()`) — aplicar la policy de la respuesta del retry.
- plan.md: DoD Fase 2 "cambios ≤ 10 s" queda verificable; nota del poll 300 s se elimina.

**Criterio de aceptación**: un cambio de política en backend se refleja en el agente en ≤ 10 s sin refresh manual (verificable con backend real o mock).

## D5 — UI inline: eliminar; /panel única UI

**Decisión**: borrar la UI inline de `/` y `/ui` (incluye el `<script>` roto con SyntaxError, `main.rs:302-513`); ambas rutas redirigen a `/panel`. El panel estático se embebe en el binario para no depender del directorio `panel/` en disco.

**Racional**: dos UIs divergieron y una se rompió. Una sola UI que mantener elimina la clase de bug. Los trays abren `/ui` y siguen funcionando vía redirect.

**Cambios**:
- `main.rs`: eliminar HTML/JS inline (~200 líneas); `/` y `/ui` → redirect a `/panel`; servir `panel/` embebido con `include_str!`/`include_bytes!` (3 archivos, sin dependencia nueva; no rust-embed) con fallback a disco vía `PANEL_DIR` para desarrollo.
- `panel/`: absorber el botón "Refrescar política" (POST `/policy/refresh`) que vivía solo en la inline.
- plan.md: P0 item de UI inline se cierra con esta decisión; Fase 5 panel item se actualiza.

**Criterio de aceptación**: `/` y `/ui` responden redirect a `/panel`; `/panel` funciona con el binario solo (sin dir `panel/`); botón refrescar política operativo en `/panel`.

## D6 — Clave AES: Keychain/DPAPI ahora

**Decisión**: la clave de la cola vive en el keystore del SO vía crate `keyring` (Keychain en macOS; Credential Manager/DPAPI en Windows), con fallback a archivo con modo 0600 si el keystore falla. Migración automática: si existe `key.bin` legacy, se importa al keystore y se elimina del disco.

**Racional**: `key.bin` plaintext world-readable junto a `queue.sqlite` (`crypto.rs:25`) derrota el cifrado en reposo. Cierra el hallazgo de seguridad nº 1 y el checkbox de la sección Seguridad.

**Cambios**:
- `agent-core`: módulo keystore (get-or-create de la clave) usado por `crypto.rs`; migración de `key.bin`.
- plan.md: hallazgo de seguridad 1 y checkbox "clave en Keychain/DPAPI" se marcan al completar.

**Criterio de aceptación**: tras arrancar, no hay clave legible en disco (salvo fallback 0600 documentado); una cola cifrada previa sigue descifrable tras la migración. Tests: roundtrip keystore y migración desde `key.bin`.

## Orden de implementación

D5 → D2 → D4 → D3 → D6 → D1 (D1 es solo plan.md). Cada decisión con unit tests donde aplica; `scripts/smoke.sh` como gate E2E al final de cada una.

## Fuera de alcance

Todo lo demás del plan (P0 winreg/agent-login-macos, GC de cola, tests transversales, packaging, Fase 4/6/7) no cambia con este spec — sigue trackeado en plan.md.
