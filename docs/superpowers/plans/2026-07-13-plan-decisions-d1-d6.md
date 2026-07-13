# Decisiones D1-D6 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implementar las 5 decisiones con código del spec `docs/superpowers/specs/2026-07-13-plan-decisions-design.md` (D5 UI, D2 heartbeat, D4 poll, D3 redacción, D6 keystore) y cerrar los checkboxes correspondientes en plan.md. D1 (Linux→v2) ya está aplicado en plan.md (commit `d0cbf28`) y no tiene tarea.

**Architecture:** Agente Rust (workspace en `/Users/gerswin/Proyectos/timeTracker`). Daemon axum en `crates/agent-daemon` (panel HTTP loopback :49219, loops de captura/heartbeat/sender/policy en `net.rs`/`capture.rs`), lógica compartida en `crates/agent-core` (cola SQLite cifrada AES-256-GCM, crypto, paths). Panel web estático en `panel/` (3 archivos). Cambios quirúrgicos: no tocar nada fuera de lo listado en cada tarea.

**Tech Stack:** Rust (axum 0.7, tokio current_thread, reqwest+rustls, rusqlite, aes-gcm, serde), keyring 3 (nuevo, solo Task 5), tempfile (dev, solo Task 5).

## Global Constraints

- **El workspace completo NO compila en macOS** (`agent-ui-windows` declara `winreg` sin cfg gate — fix fuera de alcance, es P0 aparte). SIEMPRE compilar/testear con paquetes explícitos: `cargo test -p agent-daemon -p agent-core`. NUNCA `cargo test --workspace`.
- Runtime del daemon es `current_thread`; los tests usan `#[tokio::test]` normal (no dependen del runtime del daemon).
- Nombres exactos de env vars: `POLICY_POLL_SECS`, `RIPOR_DEBUG`, `PANEL_DIR`, `API_BASE_URL`. Nombres exactos keystore: service `com.ripor.RiporAgent`, account `queue-key`.
- Mensajes de log existentes en español; mantener el estilo.
- Un commit por tarea, inmediatamente al terminar (regla del repo).
- No reformatear código ajeno; tocar solo las líneas indicadas.
- Los números de línea citados corresponden al commit `d0cbf28`; verificar contexto con el contenido mostrado antes de editar.

---

### Task 1 (D5): Eliminar UI inline; /panel embebido como única UI

**Files:**
- Modify: `crates/agent-daemon/src/main.rs` (rutas líneas 122-176, fn `ui_index` líneas 301-515)
- Modify: `panel/index.html` (sección "Política efectiva", líneas 49-58)
- Modify: `panel/app.js` (bloque `DOMContentLoaded`, líneas 109-114)
- Test: `crates/agent-daemon/src/main.rs` (módulo `#[cfg(test)]` al final del archivo)

**Interfaces:**
- Consumes: handlers existentes `/policy/refresh` (POST, ya existe en main.rs:143) y assets `panel/index.html`, `panel/app.js`, `panel/styles.css`.
- Produces: `async fn ui_redirect() -> Redirect` (307 → `/panel/`), constantes `PANEL_INDEX/PANEL_APP_JS/PANEL_CSS: &'static str`, handlers `panel_index/panel_app_js/panel_styles`. Los trays (agent-ui-macos/windows) abren `/ui` y siguen funcionando vía redirect — no se tocan.

- [ ] **Step 1: Añadir botón "Refrescar política" al panel estático**

En `panel/index.html`, dentro de la sección "Política efectiva", después de `</div>` del grid (línea 54) y antes de `<pre id="policy">—</pre>` (línea 55), insertar:

```html
        <div class="actions" style="margin:6px 0">
          <button id="btn-refresh-policy">Refrescar política</button>
        </div>
```

- [ ] **Step 2: Cablear el botón en app.js**

En `panel/app.js`, dentro del listener `DOMContentLoaded` (línea 109), después de la línea `$('btn-prompt-perms').onclick = ...` (línea 111), insertar:

```js
  const brp = $('btn-refresh-policy');
  if(brp){ brp.onclick = ()=>fetch(BASE+'/policy/refresh',{method:'POST'}).then(()=>setTimeout(refreshAll,1000)); }
```

- [ ] **Step 3: Escribir tests que fallan (redirect + assets embebidos)**

Al final de `crates/agent-daemon/src/main.rs`, añadir:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn ui_redirige_a_panel() {
        let resp = ui_redirect().await.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(resp.headers().get("location").unwrap(), "/panel/");
    }

    #[test]
    fn assets_del_panel_embebidos() {
        assert!(PANEL_INDEX.contains("RiporAgent Panel"));
        assert!(PANEL_INDEX.contains("btn-refresh-policy"));
        assert!(PANEL_APP_JS.contains("refreshAll"));
        assert!(!PANEL_CSS.is_empty());
    }
}
```

- [ ] **Step 4: Verificar que fallan**

Run: `cargo test -p agent-daemon`
Expected: FAIL — `cannot find function ui_redirect` / `cannot find value PANEL_INDEX` (error de compilación cuenta como test que falla).

- [ ] **Step 5: Implementar — borrar UI inline, añadir redirect y assets embebidos**

En `crates/agent-daemon/src/main.rs`:

5a. Borrar COMPLETA la función `async fn ui_index() -> Html<&'static str> { ... }` (líneas 301-515, desde `async fn ui_index` hasta el `}` que cierra la función, inclusive el string `HTML` con el `<script>` roto).

5b. En su lugar (misma posición del archivo), añadir:

```rust
const PANEL_INDEX: &str = include_str!("../../../panel/index.html");
const PANEL_APP_JS: &str = include_str!("../../../panel/app.js");
const PANEL_CSS: &str = include_str!("../../../panel/styles.css");

async fn ui_redirect() -> axum::response::Redirect {
    axum::response::Redirect::temporary("/panel/")
}

async fn panel_index() -> Html<&'static str> {
    Html(PANEL_INDEX)
}

async fn panel_app_js() -> impl IntoResponse {
    ([(CONTENT_TYPE, "application/javascript; charset=utf-8")], PANEL_APP_JS)
}

async fn panel_styles() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/css; charset=utf-8")], PANEL_CSS)
}
```

5c. En el Router (líneas 122-124), cambiar:

```rust
        .route("/", get(ui_index))
        .route("/ui", get(ui_index))
```

por:

```rust
        .route("/", get(ui_redirect))
        .route("/ui", get(ui_redirect))
```

5d. Reemplazar el bloque de resolución `static_dir` + `nest_service` (líneas 147-176, desde `// Resolver carpeta de panel estático` hasta el `};` que cierra `let base = if ...`) por:

```rust
    // Panel: embebido en el binario; PANEL_DIR (si existe) lo overridea para desarrollo
    let base = if let Some(static_dir) = std::env::var("PANEL_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
    {
        let svc =
            tower_http::services::ServeDir::new(static_dir).append_index_html_on_directories(true);
        base.nest_service("/panel", get_service(svc))
    } else {
        base.route("/panel", get(panel_index))
            .route("/panel/", get(panel_index))
            .route("/panel/app.js", get(panel_app_js))
            .route("/panel/styles.css", get(panel_styles))
    };
```

- [ ] **Step 6: Verificar que pasan**

Run: `cargo test -p agent-daemon`
Expected: PASS (2 tests de este task; cero errores de compilación).

- [ ] **Step 7: Verificación manual del redirect y el panel embebido**

```bash
cargo run -p agent-daemon &
sleep 3
curl -si http://127.0.0.1:49219/ui | head -3        # esperar: HTTP/1.1 307 + location: /panel/
curl -s http://127.0.0.1:49219/panel/ | grep -c 'btn-refresh-policy'   # esperar: 1
curl -si http://127.0.0.1:49219/panel/app.js | grep -i 'content-type'  # esperar: application/javascript
kill %1
```

- [ ] **Step 8: Commit**

```bash
git add crates/agent-daemon/src/main.rs panel/index.html panel/app.js
git commit -m "feat(ui): remove broken inline UI; embed /panel as single UI (D5)

/ and /ui redirect to /panel/. Panel assets embedded via include_str!
with PANEL_DIR disk override for development. Refresh-policy button
moved to the static panel."
```

---

### Task 2 (D2): Heartbeat siempre, con device_id en el body

**Files:**
- Modify: `crates/agent-daemon/src/net.rs` (fn `run_heartbeat_loop` líneas 25-93, struct `HeartbeatPayload` líneas 15-23)
- Test: `crates/agent-daemon/src/net.rs` (módulo `#[cfg(test)]` al final)

**Interfaces:**
- Consumes: `HeartbeatPayload<'a>` existente (net.rs:15-23), `MetricsHandle::get() -> AgentMetrics` (campos `cpu_pct: f32`, `mem_mb: u64`), `AgentSecrets` (campo `device_id: Option<String>`), `AgentState` (campo `device_id: String`).
- Produces: `fn heartbeat_body(device_id: &str, agent_version: &str, last_event_ts: u64, queue_len: i64, cpu_pct: f32, mem_mb: u64) -> String` — usada solo dentro de net.rs. **Nota de contrato:** el body cambia de `{status, uptime_seconds, last_activity_ms, agent_version}` a `{device_id, agent_version, last_event_ts, queue_len, cpu_pct, mem_mb}`; el backend (fuera de este repo) debe aceptar la nueva forma — documentado en plan.md en el Step 6.

- [ ] **Step 1: Escribir test que falla**

Al final de `crates/agent-daemon/src/net.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_body_incluye_device_id() {
        let body = heartbeat_body("dev-123", "0.1.0", 42, 7, 1.5, 20);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["device_id"], "dev-123");
        assert_eq!(v["agent_version"], "0.1.0");
        assert_eq!(v["last_event_ts"], 42);
        assert_eq!(v["queue_len"], 7);
        assert_eq!(v["mem_mb"], 20);
    }
}
```

- [ ] **Step 2: Verificar que falla**

Run: `cargo test -p agent-daemon heartbeat_body`
Expected: FAIL — `cannot find function heartbeat_body`.

- [ ] **Step 3: Implementar**

3a. Después del struct `HeartbeatPayload` (tras la línea 23), añadir:

```rust
fn heartbeat_body(
    device_id: &str,
    agent_version: &str,
    last_event_ts: u64,
    queue_len: i64,
    cpu_pct: f32,
    mem_mb: u64,
) -> String {
    serde_json::to_string(&HeartbeatPayload {
        device_id,
        agent_version,
        last_event_ts,
        queue_len,
        cpu_pct,
        mem_mb,
    })
    .expect("serializa heartbeat")
}
```

3b. En `run_heartbeat_loop`, BORRAR la supresión (líneas 38-40):

```rust
        if last_evt != 0 && now_ms().saturating_sub(last_evt) < 60_000 {
            continue; // hubo eventos recientes; sin heartbeat
        }
```

3c. Cambiar `let _m = metrics.get();` (línea 45) por `let m = metrics.get();`.

3d. Reemplazar la construcción del body (líneas 48-54):

```rust
                let body = serde_json::json!({
                    "status": "running",
                    "uptime_seconds": 0,
                    "last_activity_ms": last_evt,
                    "agent_version": state.agent_version,
                });
                let body_str = serde_json::to_string(&body).unwrap();
```

por:

```rust
                let body_str = heartbeat_body(
                    secrets.device_id.as_deref().unwrap_or(&state.device_id),
                    &state.agent_version,
                    last_evt,
                    queue_len,
                    m.cpu_pct,
                    m.mem_mb,
                );
```

3e. Eliminar la variable muerta `sent`: borrar `let mut sent = false;` (línea 60) y los dos `sent = true;` (líneas 67 y 77), dejando el resto de cada brazo igual (en la línea 67 queda `{ last_heartbeat_ts.store(now_ms(), Ordering::Relaxed); }` — ídem 77).

- [ ] **Step 4: Verificar que pasa**

Run: `cargo test -p agent-daemon`
Expected: PASS (tests de Task 1 + `heartbeat_body_incluye_device_id`).

- [ ] **Step 5: Commit**

```bash
git add crates/agent-daemon/src/net.rs
git commit -m "feat(heartbeat): always send every 60s with device_id in body (D2)

Remove the recent-events suppression; serialize the existing
HeartbeatPayload struct (device_id, agent_version, last_event_ts,
queue_len, cpu_pct, mem_mb) instead of the ad-hoc JSON. Drop dead
'sent' variable."
```

---

### Task 3 (D4): Policy poll 300s → POLICY_POLL_SECS (default 10) + fix retry 401

**Files:**
- Modify: `crates/agent-daemon/src/net.rs` (fn `run_policy_loop` líneas 336-385)
- Test: `crates/agent-daemon/src/net.rs` (mismo módulo `#[cfg(test)]` del Task 2)

**Interfaces:**
- Consumes: `PolicyRuntime` (`get()/set()`), `PolicyState { policy, etag }`, `save_policy(paths, &st)`, `rebootstrap(paths, &state) -> Option<AgentSecrets>` — todos existentes en net.rs/policy.rs.
- Produces: `fn poll_secs_from(v: Option<&str>) -> u64` (pura, testeable), `fn policy_poll_secs() -> u64` (lee env), `async fn apply_policy_response(resp: reqwest::Response, paths: &Paths, rt: &PolicyRuntime)` — usadas solo dentro de net.rs.

- [ ] **Step 1: Escribir tests que fallan**

Dentro del `mod tests` existente en net.rs, añadir:

```rust
    #[test]
    fn poll_secs_default_es_10() {
        assert_eq!(poll_secs_from(None), 10);
    }

    #[test]
    fn poll_secs_lee_valor_valido() {
        assert_eq!(poll_secs_from(Some("60")), 60);
    }

    #[test]
    fn poll_secs_rechaza_invalidos() {
        assert_eq!(poll_secs_from(Some("abc")), 10);
        assert_eq!(poll_secs_from(Some("0")), 10);
    }
```

- [ ] **Step 2: Verificar que fallan**

Run: `cargo test -p agent-daemon poll_secs`
Expected: FAIL — `cannot find function poll_secs_from`.

- [ ] **Step 3: Implementar**

3a. Antes de `pub async fn run_policy_loop` (línea 336), añadir:

```rust
fn poll_secs_from(v: Option<&str>) -> u64 {
    v.and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(10)
}

fn policy_poll_secs() -> u64 {
    poll_secs_from(std::env::var("POLICY_POLL_SECS").ok().as_deref())
}

async fn apply_policy_response(resp: reqwest::Response, paths: &Paths, rt: &PolicyRuntime) {
    let hdr = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    match resp.json::<serde_json::Value>().await {
        Ok(v) => {
            let polv = v.get("policy").cloned().unwrap_or(v);
            match serde_json::from_value::<crate::policy::Policy>(polv) {
                Ok(policy) => {
                    let st = PolicyState { policy, etag: hdr };
                    if let Err(e) = save_policy(paths, &st) {
                        warn!(?e, "no se pudo guardar policy");
                    }
                    rt.set(st);
                    info!("policy actualizada");
                }
                Err(e) => warn!(?e, "parse policy fallo"),
            }
        }
        Err(e) => warn!(?e, "parse json en policy fallo"),
    }
}
```

3b. En `run_policy_loop`, reemplazar el brazo de éxito (líneas 352-370, desde `Ok(resp) if resp.status().is_success() => {` hasta su `}` de cierre) por:

```rust
            Ok(resp) if resp.status().is_success() => {
                apply_policy_response(resp, paths, &rt).await;
            }
```

3c. En el brazo 401 (líneas 371-379), reemplazar `let _ = r2.send().await; // siguiente ciclo parseará` por:

```rust
                    match r2.send().await {
                        Ok(r2resp) if r2resp.status().is_success() => {
                            apply_policy_response(r2resp, paths, &rt).await;
                        }
                        Ok(r2resp) => warn!(status=?r2resp.status(), "policy tras re-bootstrap falló"),
                        Err(e2) => warn!(?e2, "policy error red tras re-bootstrap"),
                    }
```

3d. Cambiar `sleep(Duration::from_secs(300)).await;` (línea 383) por:

```rust
        sleep(Duration::from_secs(policy_poll_secs())).await;
```

- [ ] **Step 4: Verificar que pasan**

Run: `cargo test -p agent-daemon`
Expected: PASS (todos los tests acumulados).

- [ ] **Step 5: Commit**

```bash
git add crates/agent-daemon/src/net.rs
git commit -m "feat(policy): poll interval POLICY_POLL_SECS default 10s; apply 401-retry response (D4)

Extract apply_policy_response and reuse it in the success arm and the
post-rebootstrap retry (previously the retry response was discarded)."
```

---

### Task 4 (D3): Redactar títulos salvo RIPOR_DEBUG=1 (DropLog + logs)

**Files:**
- Modify: `crates/agent-daemon/src/policy.rs` (añadir `redact_title` al final, antes de tests)
- Modify: `crates/agent-daemon/src/capture.rs` (líneas 110, 123, 135, 179)
- Test: `crates/agent-daemon/src/policy.rs` (módulo `#[cfg(test)]` nuevo)

**Interfaces:**
- Consumes: `sha2::{Digest, Sha256}` y `hex::encode` (ambas deps ya presentes en agent-daemon, usadas por net.rs).
- Produces: `pub fn redact_title(title: &str) -> String` (lee env `RIPOR_DEBUG`) y `pub fn redact_title_with(debug: bool, title: &str) -> String` (pura) en `crate::policy`. Formato redactado: `[title:xxxxxxxx]` (8 hex chars de SHA-256). Título vacío queda vacío.

- [ ] **Step 1: Escribir tests que fallan**

Al final de `crates/agent-daemon/src/policy.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_oculta_titulo_sin_debug() {
        let out = redact_title_with(false, "Banco Secreto - Cuenta 1234");
        assert!(!out.contains("Banco"));
        assert!(out.starts_with("[title:"));
        assert!(out.ends_with(']'));
    }

    #[test]
    fn redact_passthrough_con_debug() {
        assert_eq!(redact_title_with(true, "hola mundo"), "hola mundo");
    }

    #[test]
    fn redact_es_determinista() {
        assert_eq!(redact_title_with(false, "x"), redact_title_with(false, "x"));
        assert_ne!(redact_title_with(false, "x"), redact_title_with(false, "y"));
    }

    #[test]
    fn redact_vacio_queda_vacio() {
        assert_eq!(redact_title_with(false, ""), "");
    }
}
```

- [ ] **Step 2: Verificar que fallan**

Run: `cargo test -p agent-daemon redact`
Expected: FAIL — `cannot find function redact_title_with`.

- [ ] **Step 3: Implementar la redacción**

En `crates/agent-daemon/src/policy.rs`, después de `impl DropLog { ... }` (tras la línea 103), añadir:

```rust
pub fn redact_title(title: &str) -> String {
    let debug = std::env::var("RIPOR_DEBUG").ok().as_deref() == Some("1");
    redact_title_with(debug, title)
}

pub fn redact_title_with(debug: bool, title: &str) -> String {
    if debug || title.is_empty() {
        return title.to_string();
    }
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(title.as_bytes());
    format!("[title:{}]", hex::encode(&h[..4]))
}
```

- [ ] **Step 4: Aplicar en capture.rs (4 sitios)**

4a. Línea 110 — cambiar:

```rust
                debug!(app = ?app, title = ?title, idle_ms, "sample actual");
```

por:

```rust
                debug!(app = ?app, title = %crate::policy::redact_title(&title), idle_ms, "sample actual");
```

4b. Línea 123 — en el `drop_log.push(...)`, cambiar `title: title.clone()` por `title: crate::policy::redact_title(&title)`.

4c. Línea 135 — en el segundo `drop_log.push(...)` (throttled), cambiar `title: effective_title.clone()` por `title: crate::policy::redact_title(&effective_title)`.

4d. Línea 179 — cambiar:

```rust
                            info!(app = ?evt.app_name, title = ?evt.window_title, "captura encolada");
```

por:

```rust
                            info!(app = ?evt.app_name, title = %crate::policy::redact_title(&evt.window_title), "captura encolada");
```

- [ ] **Step 5: Verificar que pasan**

Run: `cargo test -p agent-daemon`
Expected: PASS (todos los tests acumulados).

- [ ] **Step 6: Verificación manual de la redacción**

```bash
cargo run -p agent-daemon &
sleep 8
curl -s 'http://127.0.0.1:49219/debug/drops?limit=5'   # títulos (si hay drops) deben verse como [title:xxxxxxxx]
tail -5 "$(ls -t ~/Library/Application\ Support/com.Ripor.RiporAgent/logs/agent.log* 2>/dev/null | head -1)" 2>/dev/null | grep -o 'title=[^ ]*' | head -3
# esperar: title=[title:xxxxxxxx], nunca texto real
kill %1
```

(Si el logs dir difiere, obtenerlo del arranque del daemon: imprime la ruta del panel; el data_dir es el de `ProjectDirs::from("com","Ripor","RiporAgent")`.)

- [ ] **Step 7: Commit**

```bash
git add crates/agent-daemon/src/policy.rs crates/agent-daemon/src/capture.rs
git commit -m "feat(privacy): redact window titles in DropLog and logs unless RIPOR_DEBUG=1 (D3)

Titles are replaced by [title:<8-hex sha256>] in /debug/drops entries
and in capture debug/info traces. Full titles only with RIPOR_DEBUG=1."
```

---

### Task 5 (D6): Clave AES en keystore del SO (keyring) con fallback 0600 y migración

**Files:**
- Create: `crates/agent-core/src/keystore.rs`
- Modify: `crates/agent-core/src/lib.rs` (añadir `pub mod keystore;`)
- Modify: `crates/agent-core/src/crypto.rs` (borrar `load_or_create_key`, re-exportar la nueva)
- Modify: `crates/agent-core/Cargo.toml` (deps `keyring`, `hex`; dev-dep `tempfile`)
- Test: `crates/agent-core/src/keystore.rs` (módulo `#[cfg(test)]`) y `crates/agent-core/src/crypto.rs` (roundtrip)

**Interfaces:**
- Consumes: `Paths { pub data_dir }` (construible directo en tests), `paths.key_file() -> PathBuf`, `ensure_parent`.
- Produces: `pub trait KeyBackend { fn get(&self) -> Result<Option<Vec<u8>>>; fn set(&self, key: &[u8]) -> Result<()>; }`, `pub struct OsKeystore`, `pub fn load_or_create_key(paths: &Paths) -> Result<[u8; 32]>`, `pub fn load_or_create_key_with(backend: &dyn KeyBackend, paths: &Paths) -> Result<[u8; 32]>`. `crypto::load_or_create_key` sigue existiendo como re-export — los call sites existentes (queue) NO se tocan.

- [ ] **Step 1: Añadir dependencias**

En `crates/agent-core/Cargo.toml`, en `[dependencies]` añadir:

```toml
keyring = { version = "3", features = ["apple-native", "windows-native"] }
hex = "0.4"
```

y (crear la sección si no existe):

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Escribir tests que fallan**

Crear `crates/agent-core/src/keystore.rs` SOLO con los tests (la implementación va en Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;
    use std::sync::Mutex;

    struct MemBackend {
        store: Mutex<Option<Vec<u8>>>,
    }
    impl MemBackend {
        fn new() -> Self {
            Self { store: Mutex::new(None) }
        }
    }
    impl KeyBackend for MemBackend {
        fn get(&self) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self.store.lock().unwrap().clone())
        }
        fn set(&self, key: &[u8]) -> anyhow::Result<()> {
            *self.store.lock().unwrap() = Some(key.to_vec());
            Ok(())
        }
    }

    struct FailBackend;
    impl KeyBackend for FailBackend {
        fn get(&self) -> anyhow::Result<Option<Vec<u8>>> {
            Err(anyhow::anyhow!("sin keystore"))
        }
        fn set(&self, _key: &[u8]) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("sin keystore"))
        }
    }

    fn tmp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { data_dir: dir.path().to_path_buf() };
        (dir, paths)
    }

    #[test]
    fn genera_clave_y_la_guarda_en_keystore_sin_archivo() {
        let (_d, paths) = tmp_paths();
        let backend = MemBackend::new();
        let k1 = load_or_create_key_with(&backend, &paths).unwrap();
        assert!(!paths.key_file().exists(), "no debe crear key.bin");
        let k2 = load_or_create_key_with(&backend, &paths).unwrap();
        assert_eq!(k1, k2, "segunda llamada devuelve la misma clave");
    }

    #[test]
    fn migra_key_bin_legacy_al_keystore_y_lo_borra() {
        let (_d, paths) = tmp_paths();
        let legacy: [u8; 32] = [42u8; 32];
        std::fs::write(paths.key_file(), legacy).unwrap();
        let backend = MemBackend::new();
        let k = load_or_create_key_with(&backend, &paths).unwrap();
        assert_eq!(k, legacy, "conserva la clave legacy");
        assert!(!paths.key_file().exists(), "key.bin debe eliminarse tras migrar");
        assert_eq!(backend.get().unwrap().unwrap(), legacy.to_vec());
    }

    #[test]
    fn fallback_a_archivo_0600_si_keystore_falla() {
        let (_d, paths) = tmp_paths();
        let k1 = load_or_create_key_with(&FailBackend, &paths).unwrap();
        assert!(paths.key_file().exists(), "fallback escribe archivo");
        let k2 = load_or_create_key_with(&FailBackend, &paths).unwrap();
        assert_eq!(k1, k2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.key_file()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "archivo debe ser 0600");
        }
    }

    #[test]
    fn clave_existente_en_keystore_se_reusa_sin_tocar_disco() {
        let (_d, paths) = tmp_paths();
        let backend = MemBackend::new();
        let existente: [u8; 32] = [7u8; 32];
        backend.set(&existente).unwrap();
        let k = load_or_create_key_with(&backend, &paths).unwrap();
        assert_eq!(k, existente);
        assert!(!paths.key_file().exists());
    }
}
```

Y en `crates/agent-core/src/lib.rs` añadir `pub mod keystore;` junto a los demás `pub mod`.

- [ ] **Step 3: Verificar que fallan**

Run: `cargo test -p agent-core keystore`
Expected: FAIL — `cannot find trait KeyBackend` / `cannot find function load_or_create_key_with`.

- [ ] **Step 4: Implementar keystore.rs**

Añadir ANTES del `#[cfg(test)]` en `crates/agent-core/src/keystore.rs`:

```rust
use crate::paths::{ensure_parent, Paths};
use anyhow::{anyhow, Result};
use rand::RngCore;
use std::fs;

pub const KEY_LEN: usize = 32;
const SERVICE: &str = "com.ripor.RiporAgent";
const ACCOUNT: &str = "queue-key";

pub trait KeyBackend {
    fn get(&self) -> Result<Option<Vec<u8>>>;
    fn set(&self, key: &[u8]) -> Result<()>;
}

pub struct OsKeystore;

impl KeyBackend for OsKeystore {
    fn get(&self) -> Result<Option<Vec<u8>>> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| anyhow!("keystore: {e}"))?;
        match entry.get_password() {
            Ok(hexkey) => Ok(Some(hex::decode(hexkey)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!("keystore get: {e}")),
        }
    }
    fn set(&self, key: &[u8]) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| anyhow!("keystore: {e}"))?;
        entry
            .set_password(&hex::encode(key))
            .map_err(|e| anyhow!("keystore set: {e}"))
    }
}

pub fn load_or_create_key(paths: &Paths) -> Result<[u8; KEY_LEN]> {
    load_or_create_key_with(&OsKeystore, paths)
}

pub fn load_or_create_key_with(backend: &dyn KeyBackend, paths: &Paths) -> Result<[u8; KEY_LEN]> {
    // 1) clave ya en keystore
    match backend.get() {
        Ok(Some(k)) if k.len() == KEY_LEN => {
            let mut out = [0u8; KEY_LEN];
            out.copy_from_slice(&k);
            return Ok(out);
        }
        Ok(Some(_)) => return Err(anyhow!("clave en keystore con tamaño inválido")),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(?e, "keystore no disponible; fallback a archivo 0600");
            return file_key_0600(paths);
        }
    }
    // 2) migración desde key.bin legacy
    let legacy = paths.key_file();
    if legacy.exists() {
        let data = fs::read(&legacy)?;
        if data.len() != KEY_LEN {
            return Err(anyhow!("tamaño de clave legacy inválido"));
        }
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(&data);
        if let Err(e) = backend.set(&k) {
            tracing::warn!(?e, "keystore set falló; se mantiene key.bin");
            return Ok(k);
        }
        fs::remove_file(&legacy)?;
        tracing::info!("clave migrada de key.bin al keystore del SO");
        return Ok(k);
    }
    // 3) generar nueva
    let mut k = [0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut k);
    if let Err(e) = backend.set(&k) {
        tracing::warn!(?e, "keystore set falló; fallback a archivo 0600");
        return file_key_0600(paths);
    }
    Ok(k)
}

fn file_key_0600(paths: &Paths) -> Result<[u8; KEY_LEN]> {
    let key_path = paths.key_file();
    if key_path.exists() {
        let data = fs::read(&key_path)?;
        if data.len() != KEY_LEN {
            return Err(anyhow!("tamaño de clave inválido"));
        }
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(&data);
        return Ok(k);
    }
    let mut k = [0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut k);
    ensure_parent(&key_path)?;
    fs::write(&key_path, &k)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(k)
}
```

- [ ] **Step 5: Redirigir crypto.rs al keystore**

En `crates/agent-core/src/crypto.rs`:

5a. Borrar la función `pub fn load_or_create_key(paths: &Paths) -> Result<[u8; KEY_LEN]> { ... }` (líneas 11-27).

5b. Cambiar la línea de imports `use crate::paths::{ensure_parent, Paths};` por (ya no se usan aquí):

```rust
pub use crate::keystore::load_or_create_key;
```

5c. Borrar `use rand::RngCore;` y `use std::fs;` si quedan sin uso (el compilador avisará; `rand` sigue usándose en `encrypt_compress` para el nonce — conservar `use rand::RngCore;`).

- [ ] **Step 6: Test de roundtrip en crypto.rs**

Al final de `crates/agent-core/src/crypto.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_cifrar_descifrar() {
        let key = [7u8; 32];
        let aad = b"device-1";
        let plain = br#"{"hola":true}"#;
        let blob = encrypt_compress(&key, aad, plain).unwrap();
        let out = decrypt_decompress(&key, aad, &blob).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn descifrar_falla_con_aad_distinto() {
        let key = [7u8; 32];
        let blob = encrypt_compress(&key, b"a", b"x").unwrap();
        assert!(decrypt_decompress(&key, b"b", &blob).is_err());
    }
}
```

- [ ] **Step 7: Verificar que pasan**

Run: `cargo test -p agent-core`
Expected: PASS — 4 tests keystore + 2 tests crypto.

Run: `cargo check -p agent-daemon`
Expected: compila (los call sites de `crypto::load_or_create_key` siguen resolviendo vía re-export).

- [ ] **Step 8: Verificación manual de migración en esta máquina**

```bash
DATA_DIR="$HOME/Library/Application Support/com.Ripor.RiporAgent"
ls -la "$DATA_DIR/key.bin" 2>/dev/null   # anotar si existe (instalación previa)
cargo run -p agent-daemon &
sleep 5
kill %1
ls -la "$DATA_DIR/key.bin" 2>/dev/null   # esperar: No such file (migrado al Keychain)
security find-generic-password -s com.ripor.RiporAgent -a queue-key >/dev/null && echo "clave en Keychain OK"
# La cola previa debe seguir legible:
curl -s http://127.0.0.1:49219/queue?limit=3 2>/dev/null || true  # (con el daemon corriendo)
```

- [ ] **Step 9: Commit**

```bash
git add crates/agent-core/Cargo.toml crates/agent-core/src/keystore.rs crates/agent-core/src/lib.rs crates/agent-core/src/crypto.rs
git commit -m "feat(security): queue AES key in OS keystore via keyring, 0600 file fallback (D6)

New agent-core keystore module (Keychain on macOS, Credential
Manager/DPAPI on Windows). Legacy key.bin migrates into the keystore
and is removed. crypto::load_or_create_key re-exported unchanged for
existing call sites. Tests cover generate, reuse, migration and the
0600 fallback."
```

---

### Task 6: Cerrar checkboxes en plan.md + gate E2E

**Files:**
- Modify: `plan.md`
- No test file (documentación); gate: `scripts/smoke.sh` + suite completa.

**Interfaces:**
- Consumes: los commits de Tasks 1-5.
- Produces: plan.md consistente con el código; ningún consumidor de código.

- [ ] **Step 1: Suite completa + smoke E2E**

```bash
cargo test -p agent-core -p agent-daemon
bash scripts/smoke.sh
```

Expected: todos los tests PASS; smoke termina OK (build + /healthz + /state + transición de actividad).

- [ ] **Step 2: Actualizar plan.md**

Aplicar estos cambios de estado (buscar cada texto y editar en sitio):

1. Sección P0, item UI inline → `- [x]` y nota final "(hecho: `/` y `/ui` redirigen, panel embebido)".
2. Sección Seguridad, item clave AES → `- [x]` con nota "(keystore vía keyring + fallback 0600 + migración key.bin)".
3. Sección Seguridad, item títulos en logs → `- [x]` (redacción salvo RIPOR_DEBUG=1).
4. Sección Seguridad, item /debug/drops → `- [x]` (DropLog redactado por defecto).
5. Fase 1, item "Decidido (D2): heartbeat SIEMPRE" → `- [x]`.
6. Fase 1, nota del batch sender: sin cambios.
7. Fase 2 DoD "Cambios de política ≤ 10 s" → mantener `- [ ]` pero actualizar nota a "(implementado POLICY_POLL_SECS=10 + fix retry 401; falta verificación E2E con backend real)".
8. SLO "Aplicación de política ≤ 10 s" → actualizar nota igual que el punto 7.
9. Fase 5, item "Panel local completo" → actualizar nota: la UI inline ya no existe; `/panel` embebido es la única UI.
10. Añadir al "Inventario real de superficie": env vars nuevas `POLICY_POLL_SECS`, `RIPOR_DEBUG`.

- [ ] **Step 3: Commit final**

```bash
git add plan.md
git commit -m "plan: mark D2-D6 implemented; update env inventory

D1 was already applied in d0cbf28. Policy <=10s DoD stays open pending
E2E verification against a real backend."
```

---

## Self-review (hecho al escribir)

- **Cobertura del spec:** D5→Task 1, D2→Task 2, D4→Task 3, D3→Task 4, D6→Task 5, D1→ya aplicado (sin tarea, anotado en Goal). Criterios de aceptación del spec cubiertos por los steps de verificación manual (Task 1 Step 7, Task 4 Step 6, Task 5 Step 8) y los tests.
- **Placeholders:** ninguno; todo step con código lo incluye completo.
- **Consistencia de tipos:** `heartbeat_body` usa los campos exactos de `HeartbeatPayload<'a>` (net.rs:15-23); `redact_title` vive en `crate::policy` y capture.rs la llama con ese path; `load_or_create_key_with(&dyn KeyBackend, &Paths)` coincide entre tests (Step 2) e implementación (Step 4); `Paths { data_dir }` es construible porque el campo es `pub` (paths.rs:12).
- **Nota de riesgo consciente:** el cambio de shape del heartbeat (Task 2) requiere que el backend acepte el nuevo body — el backend no vive en este repo; queda anotado en plan.md y en el mensaje de commit.
