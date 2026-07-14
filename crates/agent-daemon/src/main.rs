use agent_core::metrics::{AgentMetrics, MetricsHandle};
use agent_core::paths::Paths;
use agent_core::state::AgentState;
use agent_core::DEFAULT_PANEL_ADDR;
use anyhow::Result;
use axum::extract::{Query, State as AxumState};
use axum::response::Html;
use axum::routing::{get, get_service, post};
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::Json;
use axum::Router;
// use axum::routing::get as ax_get;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;

mod capture;
mod policy;
#[cfg(target_os = "macos")]
mod macos_perms;
mod net;

#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
extern "C" {
    fn NSApplicationLoad() -> bool;
}

#[cfg(target_os = "macos")]
unsafe fn macos_load_appkit() {
    // Garantiza que AppKit esté cargado antes de usar clases como NSWorkspace.
    let _ = NSApplicationLoad();
}

#[derive(Clone)]
struct AppCtx {
    state: Arc<AgentState>,
    paths: agent_core::paths::Paths,
    metrics: MetricsHandle,
    version: String,
    last_event_ts: Arc<AtomicU64>,
    last_heartbeat_ts: Arc<AtomicU64>,
    last_idle_ms: Arc<AtomicU64>,
    paused_until_ms: Arc<AtomicU64>,
    policy_rt: std::sync::Arc<policy::PolicyRuntime>,
    dropped_events: Arc<AtomicU64>,
    drop_counters: std::sync::Arc<policy::DropCounters>,
    drop_log: std::sync::Arc<policy::DropLog>,
    focus_agg: std::sync::Arc<capture::FocusAgg>,
}

#[derive(Serialize)]
struct Healthz {
    ok: bool,
    version: String,
}

#[derive(Serialize)]
struct StateDto {
    device_id: String,
    agent_version: String,
    queue_len: i64,
    cpu_pct: f32,
    mem_mb: u64,
    last_event_ts: u64,
    last_heartbeat_ts: u64,
    input_idle_ms: u64,
    activity_state: String,
    paused_until_ms: u64,
    queue_preview: Vec<serde_json::Value>,
    perms: serde_json::Value,
    agent_path: String,
    policy: serde_json::Value,
    policy_etag: Option<String>,
    dropped_events: u64,
    dropped_by_reason: serde_json::Value,
    focus_blocks: Vec<capture::FocusBlockDto>,
}

// Usamos runtime de un solo hilo para garantizar que las llamadas a AppKit/AX
// se ejecuten en el hilo principal (requerido por macOS para APIs de UI).
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Carga variables desde .env si existe
    let _ = dotenvy::dotenv();
    let paths = Paths::new()?;
    let _guard = init_tracing(&paths);
    #[cfg(target_os = "macos")]
    unsafe {
        macos_load_appkit();
    }
    let version = env!("CARGO_PKG_VERSION").to_string();
    let state = AgentState::load_or_init(&paths, &version)?;

    let metrics = MetricsHandle::new();
    let metrics_bg = metrics.clone();
    tokio::spawn(async move { metrics_bg.run_sampler().await });

    let ctx = AppCtx {
        state: Arc::new(state),
        paths,
        metrics: metrics.clone(),
        version: version.clone(),
        last_event_ts: Arc::new(AtomicU64::new(0)),
        last_heartbeat_ts: Arc::new(AtomicU64::new(0)),
        last_idle_ms: Arc::new(AtomicU64::new(0)),
        paused_until_ms: Arc::new(AtomicU64::new(0)),
        policy_rt: policy::PolicyRuntime::new(),
        dropped_events: Arc::new(AtomicU64::new(0)),
        drop_counters: std::sync::Arc::new(policy::DropCounters::default()),
        drop_log: policy::DropLog::new(200),
        focus_agg: capture::FocusAgg::new(),
    };

    let app_ctx = ctx.clone();
    let base = Router::new()
        .route("/", get(ui_redirect))
        .route("/ui", get(ui_redirect))
        .route("/healthz", get(healthz))
        .route("/state", get(state_handler))
        .route("/queue", get(queue_handler))
        .route("/debug/drops", get(debug_drops_handler))
        .route("/pause", get(pause_handler))
        .route("/pause/clear", get(pause_clear_handler))
        .route("/permissions", get(perms_handler))
        .route("/permissions/prompt", get(perms_prompt_handler))
        .route(
            "/permissions/open/accessibility",
            get(perms_open_accessibility),
        )
        .route("/permissions/open/screen", get(perms_open_screen))
        .route("/debug/sample", get(debug_sample_handler))
        .route("/debug/windows", get(debug_windows_handler))
        .route("/debug/window", get(debug_windows_handler))
        .route("/debug/frontmost", get(debug_frontmost_handler))
        .route("/policy/apply", post(policy_apply_handler))
        .route("/policy/refresh", post(policy_refresh_handler))
        .route("/focus/blocks", get(focus_blocks_handler))
        .route("/focus/aggregate", get(focus_aggregate_handler))
        .route("/focus/aggregate.csv", get(focus_aggregate_csv_handler));
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
    let app = base.with_state(app_ctx);

    // Aviso temprano de permisos en macOS para ayudar a la configuración inicial
    #[cfg(target_os = "macos")]
    {
        let perms = crate::macos_perms::check_permissions();
        if !perms.accessibility_ok || !perms.screen_recording_ok {
            tracing::info!(
                ?perms,
                "permisos macOS incompletos; la captura de títulos puede ser limitada"
            );
            println!(
                "[hint] Revisa permisos en http://127.0.0.1:49219/permissions y, si falta alguno, abre http://127.0.0.1:49219/permissions/prompt"
            );
            // Prompt automático (se puede desactivar con RIPOR_NO_AUTO_PROMPT=1)
            if std::env::var("RIPOR_NO_AUTO_PROMPT").ok().as_deref() != Some("1") {
                let new_perms = crate::macos_perms::prompt_permissions();
                tracing::info!(?new_perms, "prompt automático de permisos lanzado");
                println!("[hint] Se solicitó automáticamente Accessibility y se abrió Screen Recording en System Settings");
                // Rechequeo automático 15s después, sin bloquear el arranque
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    let perms2 = crate::macos_perms::check_permissions();
                    tracing::info!(?perms2, "revisión de permisos tras prompt");
                    if !perms2.accessibility_ok || !perms2.screen_recording_ok {
                        println!(
                            "[hint] Aún faltan permisos: visita http://127.0.0.1:49219/permissions y habilita el binario en Accessibility/Screen Recording"
                        );
                    } else {
                        println!("[ok] Permisos macOS concedidos: captura más fiable disponible");
                    }
                });
            } else {
                tracing::info!("auto-prompt desactivado por RIPOR_NO_AUTO_PROMPT");
            }
        }
    }

    // Bootstrap (login) si es necesario
    {
        let s_paths = ctx.paths.clone();
        let s_state = ctx.state.clone();
        tokio::spawn(async move { net::bootstrap_if_needed(&s_paths, &s_state).await; });
    }

    // lanzar tareas de captura y heartbeat antes de iniciar servidor
    info!("spawning capture and heartbeat tasks");
    println!("[debug] spawning capture/heartbeat tasks");
    // debug: se puede verificar la captura con logs del loop
    let bg_state1 = ctx.state.clone();
    let bg_paths1 = ctx.paths.clone();
    let last_event1 = ctx.last_event_ts.clone();
    let last_idle1 = ctx.last_idle_ms.clone();
    let paused1 = ctx.paused_until_ms.clone();
    let pol1 = ctx.policy_rt.clone();
    let dropped1 = ctx.dropped_events.clone();
    let dropc1 = ctx.drop_counters.clone();
    let droplog1 = ctx.drop_log.clone();
    let focus1 = ctx.focus_agg.clone();
    tokio::spawn(async move { capture::run_capture_loop(bg_state1.clone(), &bg_paths1, last_event1, last_idle1, paused1, pol1, dropped1, dropc1, droplog1, focus1).await; });
    let bg_state2 = ctx.state.clone();
    let bg_paths2 = ctx.paths.clone();
    let bg_metrics2 = ctx.metrics.clone();
    let last_event2 = ctx.last_event_ts.clone();
    let last_hb2 = ctx.last_heartbeat_ts.clone();
    tokio::spawn(async move {
        net::run_heartbeat_loop(
            bg_state2.clone(),
            &bg_paths2,
            bg_metrics2.clone(),
            last_event2,
            last_hb2,
        )
        .await;
    });

    // opcional: sender de eventos si API_BASE_URL está configurado
    if net::api_base_url().is_some() {
        let s_state = ctx.state.clone();
        let s_paths = ctx.paths.clone();
        tokio::spawn(async move {
            net::run_sender_loop(s_state.clone(), &s_paths).await;
        });
        // policy fetch loop
        let p_paths = ctx.paths.clone();
        let prt = ctx.policy_rt.clone();
        tokio::spawn(async move { net::run_policy_loop(&p_paths, prt).await; });
    }

    let allow_external_panel =
        std::env::var("RIPOR_ALLOW_EXTERNAL_PANEL").ok().as_deref() == Some("1");
    let addr_str = std::env::var("PANEL_ADDR").unwrap_or_else(|_| DEFAULT_PANEL_ADDR.to_string());
    let mut addr: SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(addr = %addr_str, error = %e, "PANEL_ADDR inválido. Usa formato host:puerto, p.ej. 127.0.0.1:49219");
            eprintln!("[error] PANEL_ADDR inválido '{}': {}", addr_str, e);
            return Err(anyhow::anyhow!("PANEL_ADDR inválido"));
        }
    };
    if !panel_bind_is_allowed(&addr, allow_external_panel) {
        tracing::error!(
            addr = %addr,
            fallback = %DEFAULT_PANEL_ADDR,
            "PANEL_ADDR no-loopback rechazado: falta RIPOR_ALLOW_EXTERNAL_PANEL=1; usando dirección por defecto"
        );
        eprintln!(
            "[error] El panel se niega a escuchar en {} (no es loopback) sin RIPOR_ALLOW_EXTERNAL_PANEL=1. Usando {} en su lugar.",
            addr, DEFAULT_PANEL_ADDR
        );
        addr = DEFAULT_PANEL_ADDR
            .parse()
            .expect("DEFAULT_PANEL_ADDR es una dirección válida");
    } else if allow_external_panel {
        tracing::info!(addr = %addr, "RIPOR_ALLOW_EXTERNAL_PANEL=1: guard de Host desactivado, bind externo permitido explícitamente");
    }
    let bind_ip = addr.ip().to_string();
    let app = app.layer(axum::middleware::from_fn(move |req, next| {
        panel_host_guard(bind_ip.clone(), allow_external_panel, req, next)
    }));
    info!("panel escuchando en http://{}", addr);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            let kind = e.kind();
            tracing::error!(addr=%addr, ?kind, error=%e, "No se pudo abrir el puerto; ¿PANEL_ADDR en uso u ocupado por otro proceso?");
            eprintln!("[error] No se pudo abrir {}: {} (kind={:?}). Verifica procesos en el puerto o ajusta PANEL_ADDR en .env", addr, e, kind);
            return Err(e.into());
        }
    };

    let server =
        axum::serve(listener, app.into_make_service()).with_graceful_shutdown(shutdown_signal());
    if let Err(e) = server.await {
        error!(?e, "falló servidor panel");
    }
    Ok(())
}

async fn healthz(AxumState(ctx): AxumState<AppCtx>) -> Json<Healthz> {
    Json(Healthz {
        ok: true,
        version: ctx.version.clone(),
    })
}

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

async fn state_handler(AxumState(ctx): AxumState<AppCtx>) -> Json<StateDto> {
    let metrics: AgentMetrics = ctx.metrics.get();
    // abrir la cola solo para consultar la longitud
    let (queue_len, queue_preview) = match agent_core::queue::Queue::open(&ctx.paths, &ctx.state) {
        Ok(q) => {
            let len = q.queue_len().unwrap_or(0);
            // Mostrar los 5 más recientes
            let dec = q.peek_decrypted_desc(5).unwrap_or_default();
            let mut items = Vec::new();
            for b in dec {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) {
                    items.push(v);
                } else {
                    items.push(serde_json::json!({"raw": base64::engine::general_purpose::STANDARD.encode(b)}));
                }
            }
            (len, items)
        }
        Err(_) => (0, Vec::new()),
    };
    // permisos
    #[cfg(target_os = "macos")]
    let perms_v = serde_json::to_value(crate::macos_perms::check_permissions()).unwrap();
    #[cfg(not(target_os = "macos"))]
    let perms_v = serde_json::json!({"unsupported": true});

    let dc = &ctx.drop_counters;
    Json(StateDto {
        device_id: ctx.state.device_id.clone(),
        agent_version: ctx.state.agent_version.clone(),
        queue_len,
        cpu_pct: metrics.cpu_pct,
        mem_mb: metrics.mem_mb,
        last_event_ts: ctx.last_event_ts.load(Ordering::Relaxed),
        last_heartbeat_ts: ctx.last_heartbeat_ts.load(Ordering::Relaxed),
        input_idle_ms: ctx.last_idle_ms.load(Ordering::Relaxed),
        activity_state: derive_activity_state(ctx.last_idle_ms.load(Ordering::Relaxed)),
        paused_until_ms: ctx.paused_until_ms.load(Ordering::Relaxed),
        queue_preview,
        perms: perms_v,
        agent_path: std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
        policy: serde_json::to_value(ctx.policy_rt.get().policy).unwrap_or(serde_json::json!({})),
        policy_etag: ctx.policy_rt.get().etag,
        dropped_events: ctx.dropped_events.load(Ordering::Relaxed),
        dropped_by_reason: serde_json::json!({
            "killSwitch": dc.kill_switch.load(Ordering::Relaxed),
            "pauseCapture": dc.pause.load(Ordering::Relaxed),
            "excludedApp": dc.excluded_app.load(Ordering::Relaxed),
            "excludedPattern": dc.excluded_pattern.load(Ordering::Relaxed),
            "excludedExePath": dc.excluded_exe_path.load(Ordering::Relaxed),
            "throttled": dc.throttled.load(Ordering::Relaxed),
        }),
        focus_blocks: ctx.focus_agg.recent(5, ctx.policy_rt.get().policy.focusMinMinutes.unwrap_or(5)),
    })
}

#[derive(Deserialize)]
struct DropsParams { limit: Option<usize> }

async fn debug_drops_handler(AxumState(ctx): AxumState<AppCtx>, Query(p): Query<DropsParams>) -> Json<serde_json::Value> {
    let limit = p.limit.unwrap_or(50).min(500);
    let items = ctx.drop_log.list_desc(limit);
    Json(serde_json::json!({ "total": items.len(), "items": items }))
}

async fn policy_apply_handler(AxumState(ctx): AxumState<AppCtx>, axum::Json(body): axum::Json<serde_json::Value>) -> Json<serde_json::Value> {
    // admitir envoltura {policy:{...}}
    let pol_v = body.get("policy").cloned().unwrap_or(body);
    match serde_json::from_value::<crate::policy::Policy>(pol_v) {
        Ok(policy) => {
            let st = crate::policy::PolicyState { policy, etag: None };
            if let Err(e) = crate::policy::save_policy(&ctx.paths, &st) {
                return Json(serde_json::json!({"ok": false, "error": format!("save failed: {}", e)}));
            }
            ctx.policy_rt.set(st);
            Json(serde_json::json!({"ok": true}))
        }
        Err(e) => Json(serde_json::json!({"ok": false, "error": format!("parse failed: {}", e)})),
    }
}

async fn policy_refresh_handler(AxumState(ctx): AxumState<AppCtx>) -> Json<serde_json::Value> {
    let p = ctx.paths.clone();
    let rt = ctx.policy_rt.clone();
    tokio::spawn(async move { crate::net::fetch_policy_once(&p, rt).await; });
    Json(serde_json::json!({"ok": true}))
}

#[derive(Deserialize)]
struct FocusParams { limit: Option<usize>, min_minutes: Option<u32> }

async fn focus_blocks_handler(AxumState(ctx): AxumState<AppCtx>, Query(p): Query<FocusParams>) -> Json<serde_json::Value> {
    let limit = p.limit.unwrap_or(10).min(100);
    let min_m = p.min_minutes.unwrap_or_else(|| ctx.policy_rt.get().policy.focusMinMinutes.unwrap_or(5));
    let mut items_json = Vec::new();
    if let Ok(store) = agent_core::focus::FocusStore::open(&ctx.paths) {
        if let Ok(rows) = store.list_recent(limit, 0) {
            for r in rows {
                if (r.dur_ms as u64) >= (min_m as u64).saturating_mul(60_000) {
                    items_json.push(serde_json::json!({
                        "app_name": r.app_name,
                        "window_title": r.window_title,
                        "start_ms": r.start_ms,
                        "end_ms": r.end_ms,
                        "dur_ms": r.dur_ms,
                    }));
                }
            }
        }
    }
    if items_json.is_empty() {
        let prev = ctx.focus_agg.recent(limit, min_m);
        for b in prev {
            items_json.push(serde_json::json!({
                "app_name": b.app_name,
                "window_title": b.window_title,
                "start_ms": b.start_ms as i64,
                "end_ms": b.end_ms as i64,
                "dur_ms": b.dur_ms as i64,
            }));
        }
    }
    Json(serde_json::json!({"items": items_json}))
}

#[derive(Deserialize)]
struct FocusAggParams { days: Option<u32> }

async fn focus_aggregate_handler(AxumState(ctx): AxumState<AppCtx>, Query(p): Query<FocusAggParams>) -> Json<serde_json::Value> {
    let days = p.days.unwrap_or(7).min(90);
    let mut items: Vec<serde_json::Value> = Vec::new();
    if let Ok(store) = agent_core::focus::FocusStore::open(&ctx.paths) {
        if let Ok(rows) = store.aggregate_last_days_by_app(days) {
            for r in rows { items.push(serde_json::json!({"day": r.day, "app_name": r.app_name, "dur_ms": r.dur_ms})); }
        }
    }
    Json(serde_json::json!({"days": days, "items": items}))
}

async fn focus_aggregate_csv_handler(AxumState(ctx): AxumState<AppCtx>, Query(p): Query<FocusAggParams>) -> impl IntoResponse {
    let days = p.days.unwrap_or(7).min(90);
    let mut rows: Vec<(String, String, i64)> = Vec::new();
    if let Ok(store) = agent_core::focus::FocusStore::open(&ctx.paths) {
        if let Ok(items) = store.aggregate_last_days_by_app(days) {
            for r in items { rows.push((r.day, r.app_name, r.dur_ms)); }
        }
    }
    let mut csv = String::new();
    csv.push_str("day,app_name,dur_ms,dur_hhmm\n");
    for (day, app, dur_ms) in rows {
        let mm = (dur_ms / 60000).max(0);
        let ss = ((dur_ms % 60000) / 1000).abs();
        csv.push_str(&format!("{},{},{},{}:{:02}\n", day, escape_csv(&app), dur_ms, mm, ss));
    }
    ([ (CONTENT_TYPE, "text/csv; charset=utf-8") ], csv)
}

fn escape_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        let mut out = String::from("\"");
        for ch in s.chars() { if ch == '"' { out.push('"'); } out.push(ch); }
        out.push('"'); out
    } else { s.to_string() }
}

#[derive(Deserialize)]
struct PauseParams {
    minutes: Option<u64>,
    ms: Option<u64>,
}

async fn pause_handler(
    AxumState(ctx): AxumState<AppCtx>,
    Query(p): Query<PauseParams>,
) -> Json<serde_json::Value> {
    let now = now_ms();
    let dur_ms =
        p.ms.or(p.minutes.map(|m| m * 60_000))
            .unwrap_or(15 * 60_000);
    let until = now.saturating_add(dur_ms);
    ctx.paused_until_ms.store(until, Ordering::Relaxed);
    Json(serde_json::json!({"ok": true, "paused_until_ms": until}))
}

async fn pause_clear_handler(AxumState(ctx): AxumState<AppCtx>) -> Json<serde_json::Value> {
    ctx.paused_until_ms.store(0, Ordering::Relaxed);
    Json(serde_json::json!({"ok": true}))
}

/// True si el bind solicitado es seguro por defecto (loopback) o si el
/// operador lo permitió explícitamente vía RIPOR_ALLOW_EXTERNAL_PANEL=1.
fn panel_bind_is_allowed(addr: &SocketAddr, allow_external: bool) -> bool {
    addr.ip().is_loopback() || allow_external
}

/// True si `host` (valor de la cabecera Host, con o sin `:puerto`) es
/// loopback: "127.0.0.1", "localhost", "[::1]", con o sin puerto, o si
/// coincide con la IP a la que el panel está enlazado (`bind_ip`, p.ej.
/// "127.0.0.2" para un bind loopback no estándar). No valida el puerto: una
/// petición solo llega a este listener por el puerto realmente enlazado, así
/// que el puerto del Host no aporta seguridad. No valida esquema ni userinfo
/// (la cabecera Host no los lleva).
fn host_is_local(host: &str, bind_ip: &str) -> bool {
    let h = host.trim();
    if h.is_empty() {
        return false;
    }
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.split(':').next().unwrap_or("")
    };
    let name = name.to_ascii_lowercase();
    matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1")
        || name == bind_ip.to_ascii_lowercase()
}

/// Middleware: rechaza (403) peticiones cuya cabecera Host no sea loopback,
/// mitigando ataques de DNS-rebinding / drive-by contra endpoints que mutan
/// estado (p.ej. /policy/apply, /pause) aunque el panel esté en 127.0.0.1.
/// Se omite por completo si el operador optó por exponer el panel
/// externamente (RIPOR_ALLOW_EXTERNAL_PANEL=1).
async fn panel_host_guard(
    bind_ip: String,
    allow_external: bool,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if allow_external {
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| host_is_local(h, &bind_ip))
        .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        axum::http::StatusCode::FORBIDDEN.into_response()
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn derive_activity_state(idle_ms: u64) -> String {
    // Umbral por defecto: 60s
    let threshold_ms: u64 = std::env::var("IDLE_ACTIVE_THRESHOLD_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60_000);
    if idle_ms < threshold_ms {
        "ONLINE_ACTIVE".to_string()
    } else {
        "ONLINE_IDLE".to_string()
    }
}

#[cfg(target_os = "macos")]
async fn perms_handler() -> Json<macos_perms::PermsStatus> {
    Json(macos_perms::check_permissions())
}

#[cfg(not(target_os = "macos"))]
async fn perms_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({"unsupported": true}))
}

#[cfg(target_os = "macos")]
async fn perms_prompt_handler() -> Json<macos_perms::PermsStatus> {
    Json(macos_perms::prompt_permissions())
}

#[cfg(not(target_os = "macos"))]
async fn perms_prompt_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({"unsupported": true}))
}

#[cfg(target_os = "macos")]
async fn perms_open_accessibility() -> Json<serde_json::Value> {
    macos_perms::open_accessibility_pane();
    Json(serde_json::json!({"ok": true}))
}

#[cfg(not(target_os = "macos"))]
async fn perms_open_accessibility() -> Json<serde_json::Value> {
    Json(serde_json::json!({"unsupported": true}))
}

#[cfg(target_os = "macos")]
async fn perms_open_screen() -> Json<serde_json::Value> {
    macos_perms::open_screencapture_pane();
    Json(serde_json::json!({"ok": true}))
}

#[cfg(not(target_os = "macos"))]
async fn perms_open_screen() -> Json<serde_json::Value> {
    Json(serde_json::json!({"unsupported": true}))
}

#[derive(Serialize)]
struct QueueDto {
    queue_len: i64,
    top: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct QueueParams {
    limit: Option<usize>,
}

async fn queue_handler(
    AxumState(ctx): AxumState<AppCtx>,
    Query(params): Query<QueueParams>,
) -> Json<QueueDto> {
    let limit = params.limit.unwrap_or(10).min(100).max(1);
    let q = agent_core::queue::Queue::open(&ctx.paths, &ctx.state);
    let (len, items) = match q {
        Ok(q) => {
            let len = q.queue_len().unwrap_or(0);
            // Mostrar los últimos N en cola (más recientes primero)
            let dec = q.peek_decrypted_desc(limit).unwrap_or_default();
            let mut top = Vec::new();
            for b in dec {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) {
                    top.push(v);
                } else {
                    top.push(serde_json::json!({"raw": base64::engine::general_purpose::STANDARD.encode(b)}));
                }
            }
            (len, top)
        }
        Err(_) => (0, Vec::new()),
    };
    Json(QueueDto {
        queue_len: len,
        top: items,
    })
}

#[cfg(target_os = "macos")]
async fn debug_sample_handler() -> Json<capture::SampleDebugDto> {
    match capture::sample_debug() {
        Ok(v) => Json(v),
        Err(_) => Json(capture::SampleDebugDto {
            app_name: String::new(),
            window_title: String::new(),
            input_idle_ms: 0,
            title_source: "error".into(),
            ax_pid: None,
            ax_name: None,
            ns_pid: None,
            ns_name: None,
            cg_pid: None,
            cg_owner: None,
            cg_title: None,
            ax_title: None,
            perms: crate::macos_perms::check_permissions(),
        }),
    }
}

#[cfg(target_os = "windows")]
async fn debug_sample_handler() -> Json<capture::SampleDebugDto> {
    match capture::sample_debug() {
        Ok(v) => Json(v),
        Err(_) => Json(capture::SampleDebugDto {
            app_name: String::new(),
            window_title: String::new(),
            input_idle_ms: 0,
            title_source: "error".into(),
            ax_pid: None,
            ax_name: None,
            ns_pid: None,
            ns_name: None,
            cg_pid: None,
            cg_owner: None,
            cg_title: None,
            ax_title: None,
            #[cfg(target_os = "windows")]
            win_pid: None,
            #[cfg(target_os = "windows")]
            win_thread_id: None,
            #[cfg(target_os = "windows")]
            win_hwnd: None,
            #[cfg(target_os = "windows")]
            win_root_hwnd: None,
            #[cfg(target_os = "windows")]
            win_class: None,
            #[cfg(target_os = "windows")]
            win_process_path: None,
        }),
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
async fn debug_sample_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({"unsupported": true}))
}

#[cfg(target_os = "macos")]
async fn debug_windows_handler() -> Json<Vec<capture::WindowInfoDto>> {
    Json(capture::list_windows_debug(10))
}

#[cfg(not(target_os = "macos"))]
async fn debug_windows_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({"unsupported": true}))
}

#[cfg(target_os = "macos")]
async fn debug_frontmost_handler() -> Json<capture::FrontmostDebugDto> {
    Json(capture::frontmost_debug())
}

#[cfg(not(target_os = "macos"))]
async fn debug_frontmost_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({"unsupported": true}))
}

fn init_tracing(paths: &Paths) -> WorkerGuard {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let logs_dir = paths.logs_dir();
    std::fs::create_dir_all(&logs_dir).ok();
    let file_appender = tracing_appender::rolling::daily(&logs_dir, "agent.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let fmt_layer_file = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .with_writer(non_blocking);

    let fmt_layer_stdout = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact();

    use tracing_subscriber::prelude::*;
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(filter))
        .with(fmt_layer_stdout)
        .with(fmt_layer_file);
    subscriber.init();
    guard
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("falló instalar handler de ctrl-c");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("no se pudo instalar SIGTERM");
        term.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

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

    #[test]
    fn panel_bind_is_allowed_loopback_sin_opt_in() {
        let addr: SocketAddr = "127.0.0.1:49219".parse().unwrap();
        assert!(panel_bind_is_allowed(&addr, false));
    }

    #[test]
    fn panel_bind_is_allowed_no_loopback_sin_opt_in_rechaza() {
        let addr: SocketAddr = "0.0.0.0:49219".parse().unwrap();
        assert!(!panel_bind_is_allowed(&addr, false));
    }

    #[test]
    fn panel_bind_is_allowed_no_loopback_con_opt_in() {
        let addr: SocketAddr = "0.0.0.0:49219".parse().unwrap();
        assert!(panel_bind_is_allowed(&addr, true));
    }

    #[test]
    fn panel_bind_is_allowed_ipv6_loopback_sin_opt_in() {
        let addr: SocketAddr = "[::1]:49219".parse().unwrap();
        assert!(panel_bind_is_allowed(&addr, false));
    }

    #[test]
    fn host_is_local_ipv4_sin_puerto() {
        assert!(host_is_local("127.0.0.1", "127.0.0.1"));
    }

    #[test]
    fn host_is_local_ipv4_con_puerto() {
        assert!(host_is_local("127.0.0.1:49219", "127.0.0.1"));
    }

    #[test]
    fn host_is_local_localhost_con_puerto() {
        assert!(host_is_local("localhost:49219", "127.0.0.1"));
    }

    #[test]
    fn host_is_local_ipv6_con_puerto() {
        assert!(host_is_local("[::1]:49219", "127.0.0.1"));
    }

    #[test]
    fn host_is_local_dominio_externo_rechaza() {
        assert!(!host_is_local("evil.com", "127.0.0.1"));
    }

    #[test]
    fn host_is_local_dominio_externo_con_puerto_rechaza() {
        assert!(!host_is_local("evil.com:49219", "127.0.0.1"));
    }

    #[test]
    fn host_is_local_vacio_rechaza() {
        assert!(!host_is_local("", "127.0.0.1"));
    }

    #[test]
    fn host_is_local_subdominio_engañoso_rechaza() {
        assert!(!host_is_local("localhost.evil.com", "127.0.0.1"));
    }

    #[test]
    fn host_is_local_bind_no_estandar_acepta_su_propia_ip() {
        // Bind loopback no estándar (127.0.0.2): el guard debe aceptar
        // Host con esa IP, con o sin puerto.
        assert!(host_is_local("127.0.0.2:49219", "127.0.0.2"));
        assert!(host_is_local("127.0.0.2", "127.0.0.2"));
    }

    #[test]
    fn host_is_local_bind_no_estandar_rechaza_remoto() {
        assert!(!host_is_local("evil.com", "127.0.0.2"));
    }

    #[test]
    fn host_is_local_literal_loopback_siempre_ok() {
        // "localhost" sigue siendo válido aunque el bind sea 127.0.0.2.
        assert!(host_is_local("localhost", "127.0.0.2"));
    }

    // Verificación end-to-end del guard vía un mini-router, sin arrancar
    // main() completo (que en este entorno se bloquea en el acceso a
    // Keychain de macOS al abrir la cola; ver reporte de la tarea).
    #[tokio::test]
    async fn panel_host_guard_permite_loopback_y_rechaza_spoof() {
        use tower::ServiceExt;

        async fn ok() -> &'static str {
            "ok"
        }
        let bind_ip = "127.0.0.1".to_string();
        let allow_external = false;
        let app = Router::new().route("/x", get(ok)).layer(
            axum::middleware::from_fn(move |req, next| {
                panel_host_guard(bind_ip.clone(), allow_external, req, next)
            }),
        );

        let req_ok = axum::http::Request::builder()
            .uri("/x")
            .header("Host", "127.0.0.1:49219")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp_ok = app.clone().oneshot(req_ok).await.unwrap();
        assert_eq!(resp_ok.status(), axum::http::StatusCode::OK);

        let req_spoof = axum::http::Request::builder()
            .uri("/x")
            .header("Host", "evil.com")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp_spoof = app.clone().oneshot(req_spoof).await.unwrap();
        assert_eq!(resp_spoof.status(), axum::http::StatusCode::FORBIDDEN);

        let req_missing = axum::http::Request::builder()
            .uri("/x")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp_missing = app.clone().oneshot(req_missing).await.unwrap();
        assert_eq!(resp_missing.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn panel_host_guard_se_omite_con_allow_external() {
        use tower::ServiceExt;

        async fn ok() -> &'static str {
            "ok"
        }
        let bind_ip = "127.0.0.1".to_string();
        let allow_external = true;
        let app = Router::new().route("/x", get(ok)).layer(
            axum::middleware::from_fn(move |req, next| {
                panel_host_guard(bind_ip.clone(), allow_external, req, next)
            }),
        );

        let req_spoof = axum::http::Request::builder()
            .uri("/x")
            .header("Host", "evil.com")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp_spoof = app.clone().oneshot(req_spoof).await.unwrap();
        assert_eq!(resp_spoof.status(), axum::http::StatusCode::OK);
    }
}
