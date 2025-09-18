use agent_core::metrics::{AgentMetrics, MetricsHandle};
use agent_core::paths::Paths;
use agent_core::state::AgentState;
use agent_core::DEFAULT_PANEL_ADDR;
use anyhow::Result;
use axum::extract::{State as AxumState, Query};
use axum::routing::{get, get_service};
use axum::Json;
use axum::response::Html;
use axum::Router;
// use axum::routing::get as ax_get;
use serde::{Serialize, Deserialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::signal;
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;
use base64::Engine;

mod capture;
mod net;
#[cfg(target_os = "macos")]
mod macos_perms;

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
}

#[derive(Serialize)]
struct Healthz { ok: bool, version: String }

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
    unsafe { macos_load_appkit(); }
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
    };

    let app_ctx = ctx.clone();
    let base = Router::new()
        .route("/", get(ui_index))
        .route("/ui", get(ui_index))
        .route("/healthz", get(healthz))
        .route("/state", get(state_handler))
        .route("/queue", get(queue_handler))
        .route("/pause", get(pause_handler))
        .route("/pause/clear", get(pause_clear_handler))
        .route("/permissions", get(perms_handler))
        .route("/permissions/prompt", get(perms_prompt_handler))
        .route("/permissions/open/accessibility", get(perms_open_accessibility))
        .route("/permissions/open/screen", get(perms_open_screen))
        .route("/debug/sample", get(debug_sample_handler))
        .route("/debug/windows", get(debug_windows_handler))
        .route("/debug/window", get(debug_windows_handler))
        .route("/debug/frontmost", get(debug_frontmost_handler));
    // Resolver carpeta de panel estático: PANEL_DIR, ./panel, o ../../panel (raíz del workspace)
    let static_dir = std::env::var("PANEL_DIR").ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
        .or_else(|| {
            let p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join("panel");
            if p.exists() { Some(p) } else { None }
        })
        .or_else(|| {
            let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../panel");
            if p.exists() { Some(p) } else { None }
        });
    let base = if let Some(static_dir) = static_dir {
        let svc = tower_http::services::ServeDir::new(static_dir)
            .append_index_html_on_directories(true);
        base.nest_service("/panel", get_service(svc))
    } else { base };
    let app = base.with_state(app_ctx);

    // Aviso temprano de permisos en macOS para ayudar a la configuración inicial
    #[cfg(target_os = "macos")]
    {
        let perms = crate::macos_perms::check_permissions();
        if !perms.accessibility_ok || !perms.screen_recording_ok {
            tracing::info!(?perms, "permisos macOS incompletos; la captura de títulos puede ser limitada");
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

    // lanzar tareas de captura y heartbeat antes de iniciar servidor
    info!("spawning capture and heartbeat tasks");
    println!("[debug] spawning capture/heartbeat tasks");
    // debug: se puede verificar la captura con logs del loop
    let bg_state1 = ctx.state.clone();
    let bg_paths1 = ctx.paths.clone();
    let last_event1 = ctx.last_event_ts.clone();
    let last_idle1 = ctx.last_idle_ms.clone();
    let paused1 = ctx.paused_until_ms.clone();
    tokio::spawn(async move {
        capture::run_capture_loop(bg_state1.clone(), &bg_paths1, last_event1, last_idle1, paused1).await;
    });
    let bg_state2 = ctx.state.clone();
    let bg_paths2 = ctx.paths.clone();
    let bg_metrics2 = ctx.metrics.clone();
    let last_event2 = ctx.last_event_ts.clone();
    let last_hb2 = ctx.last_heartbeat_ts.clone();
    tokio::spawn(async move {
        net::run_heartbeat_loop(bg_state2.clone(), &bg_paths2, bg_metrics2.clone(), last_event2, last_hb2).await;
    });

    // opcional: sender de eventos si EVENTS_URL está configurado
    if std::env::var("EVENTS_URL").is_ok() {
        let s_state = ctx.state.clone();
        let s_paths = ctx.paths.clone();
        tokio::spawn(async move {
            net::run_sender_loop(s_state.clone(), &s_paths).await;
        });
    }

    let addr_str = std::env::var("PANEL_ADDR").unwrap_or_else(|_| DEFAULT_PANEL_ADDR.to_string());
    let addr: SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(addr = %addr_str, error = %e, "PANEL_ADDR inválido. Usa formato host:puerto, p.ej. 127.0.0.1:49219");
            eprintln!("[error] PANEL_ADDR inválido '{}': {}", addr_str, e);
            return Err(anyhow::anyhow!("PANEL_ADDR inválido"));
        }
    };
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

    let server = axum::serve(listener, app.into_make_service()).with_graceful_shutdown(shutdown_signal());
    if let Err(e) = server.await {
        error!(?e, "falló servidor panel");
    }
    Ok(())
}

async fn healthz(AxumState(ctx): AxumState<AppCtx>) -> Json<Healthz> {
    Json(Healthz { ok: true, version: ctx.version.clone() })
}

async fn ui_index() -> Html<&'static str> {
    const HTML: &str = r#"<!doctype html>
<html lang="es">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>RiporAgent UI</title>
    <style>
      body{font-family:system-ui,-apple-system,Segoe UI,Roboto,Ubuntu;margin:0;background:#0f1116;color:#e6e6e6}
      header{padding:12px 16px;background:#151922;border-bottom:1px solid #202534;display:flex;justify-content:space-between}
      .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:12px;padding:12px 16px}
      .card{background:#151922;padding:10px;border:1px solid #202534;border-radius:8px}
      .muted{color:#9aa3b2;font-size:12px}
      pre{background:#151922;margin:12px 16px;padding:12px;border-radius:8px;border:1px solid #202534;max-height:320px;overflow:auto}
      .ok{color:#22c55e}.warn{color:#eab308}.bad{color:#ef4444}
    </style>
  </head>
  <body>
    <header><h1>RiporAgent</h1><span id="ver"></span></header>
    <div class="grid">
      <div class="card"><div class="muted">Device ID</div><div id="device"></div></div>
      <div class="card"><div class="muted">CPU %</div><div id="cpu"></div></div>
      <div class="card"><div class="muted">RAM MB</div><div id="mem"></div></div>
      <div class="card"><div class="muted">Idle ms</div><div id="idle"></div></div>
      <div class="card"><div class="muted">Actividad</div><div id="act"></div></div>
      <div class="card"><div class="muted">Monitoreo</div><div id="mon"></div></div>
      <div class="card"><div class="muted">Cola</div><div id="qlen"></div></div>
    </div>
    <div class="card" style="margin:0 16px"><div class="muted">Permisos</div><div id="perms">—</div>
      <div class="muted" style="margin-top:6px">Binario a autorizar:</div>
      <div><code id="agent_path"></code></div>
      <div style="margin-top:8px;display:flex;gap:8px;flex-wrap:wrap">
        <button id="prompt">Solicitar permisos</button>
        <button id="openAx">Abrir Accesibilidad</button>
        <button id="openSc">Abrir Screen Recording</button>
      </div>
    </div>
    <div class="card" style="margin:12px 16px"><div class="muted">Foco</div><div id="focus_consistency">—</div><pre id="focus">—</pre></div>
    <pre id="queue">—</pre>
    <script>
      async function j(u){const r=await fetch(u,{cache:'no-store'});if(!r.ok)throw new Error(u+':'+r.status);return r.json()}
      async function ref(){
        try{const s=await j('/state');
          document.getElementById('ver').textContent='v'+s.agent_version;
          document.getElementById('device').textContent=s.device_id;
          document.getElementById('cpu').textContent=s.cpu_pct.toFixed(2);
          document.getElementById('mem').textContent=s.mem_mb;
          document.getElementById('idle').textContent=s.input_idle_ms;
          document.getElementById('act').textContent=s.activity_state;
          document.getElementById('qlen').textContent=s.queue_len;
          document.getElementById('perms').innerHTML = s.perms && s.perms.unsupported ? 'No aplica' :
            'Accessibility: <b>'+s.perms.accessibility_ok+'</b> — Screen Recording: <b>'+s.perms.screen_recording_ok+'</b>';
          document.getElementById('agent_path').textContent=s.agent_path;
          const mon = document.getElementById('mon');
          if(s.paused_until_ms && s.paused_until_ms > 0){
            const d=new Date(Number(s.paused_until_ms)); mon.className='warn'; mon.textContent='Pausado hasta '+d.toLocaleTimeString();
          } else { mon.className='ok'; mon.textContent='Monitoreo activo'; }
        }catch(e){ console.error('state', e); }
        try{const q=await j('/queue?limit=10'); document.getElementById('queue').textContent=JSON.stringify(q.top,null,2);}catch(e){ console.error('queue', e); }
        try{const f=await j('/debug/sample');
          document.getElementById('focus').textContent=JSON.stringify({app_name:f.app_name, ax_name:f.ax_name, ns_name:f.ns_name, cg_owner:f.cg_owner, title_source:f.title_source, window_title:f.window_title, cg_title:f.cg_title, ax_title:f.ax_title},null,2);
          const names=[f.ax_name,f.ns_name,f.cg_owner].filter(Boolean);
          const allEq=names.length>0 && names.every(n=>n===names[0]);
          const el=document.getElementById('focus_consistency');
          if(allEq){ el.className='ok'; el.textContent='OK: AX/NS/CG concuerdan ('+names[0]+')'; }
          else { el.className='warn'; el.textContent='ATENCIÓN: fuentes difieren — AX='+(f.ax_name||'—')+' / NS='+(f.ns_name||'—')+' / CG='+(f.cg_owner||'—'); }
        }catch(e){ console.error('sample', e); }
      }
      document.addEventListener('DOMContentLoaded',()=>{
        const btn=document.getElementById('prompt');
        if(btn){ btn.onclick=()=>j('/permissions/prompt').then(()=>setTimeout(ref,1500)); }
        const bax=document.getElementById('openAx'); if(bax){ bax.onclick=()=>j('/permissions/open/accessibility').then(()=>setTimeout(ref,1000)); }
        const bsc=document.getElementById('openSc'); if(bsc){ bsc.onclick=()=>j('/permissions/open/screen').then(()=>setTimeout(ref,1000)); }
        ref(); setInterval(ref,2000);
      });
    </script>
  </body>
</html>
"#;
    Html(HTML)
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
    })
}

#[derive(Deserialize)]
struct PauseParams { minutes: Option<u64>, ms: Option<u64> }

async fn pause_handler(AxumState(ctx): AxumState<AppCtx>, Query(p): Query<PauseParams>) -> Json<serde_json::Value> {
    let now = now_ms();
    let dur_ms = p.ms.or(p.minutes.map(|m| m*60_000)).unwrap_or(15*60_000);
    let until = now.saturating_add(dur_ms);
    ctx.paused_until_ms.store(until, Ordering::Relaxed);
    Json(serde_json::json!({"ok": true, "paused_until_ms": until}))
}

async fn pause_clear_handler(AxumState(ctx): AxumState<AppCtx>) -> Json<serde_json::Value> {
    ctx.paused_until_ms.store(0, Ordering::Relaxed);
    Json(serde_json::json!({"ok": true}))
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn derive_activity_state(idle_ms: u64) -> String {
    // Umbral por defecto: 60s
    let threshold_ms: u64 = std::env::var("IDLE_ACTIVE_THRESHOLD_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60_000);
    if idle_ms < threshold_ms { "ONLINE_ACTIVE".to_string() } else { "ONLINE_IDLE".to_string() }
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
struct QueueParams { limit: Option<usize> }

async fn queue_handler(AxumState(ctx): AxumState<AppCtx>, Query(params): Query<QueueParams>) -> Json<QueueDto> {
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
    Json(QueueDto { queue_len: len, top: items })
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
        })
    }
}

#[cfg(not(target_os = "macos"))]
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
