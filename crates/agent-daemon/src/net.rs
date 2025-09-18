use agent_core::metrics::MetricsHandle;
use agent_core::paths::Paths;
use agent_core::state::AgentState;
use reqwest::Client;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use base64::Engine;

#[derive(Serialize)]
struct HeartbeatPayload<'a> {
    device_id: &'a str,
    agent_version: &'a str,
    last_event_ts: u64,
    queue_len: i64,
    cpu_pct: f32,
    mem_mb: u64,
}

pub async fn run_heartbeat_loop(
    state: Arc<AgentState>,
    paths: &Paths,
    metrics: MetricsHandle,
    last_event_ts: Arc<AtomicU64>,
    last_heartbeat_ts: Arc<AtomicU64>,
) {
    info!("iniciando loop de heartbeat (Fase 1)");
    let client = Client::builder().build().expect("client http");
    let hb_url = std::env::var("HEARTBEAT_URL").ok();
    loop {
        sleep(Duration::from_secs(60)).await;
        let last_evt = last_event_ts.load(Ordering::Relaxed);
        if last_evt != 0 && now_ms().saturating_sub(last_evt) < 60_000 {
            continue; // hubo eventos recientes; sin heartbeat
        }
        let queue_len = match agent_core::queue::Queue::open(paths, &state) {
            Ok(q) => q.queue_len().unwrap_or(0),
            Err(_) => 0,
        };
        let m = metrics.get();
        let payload = HeartbeatPayload {
            device_id: &state.device_id,
            agent_version: &state.agent_version,
            last_event_ts: last_evt,
            queue_len,
            cpu_pct: m.cpu_pct,
            mem_mb: m.mem_mb,
        };
        if let Some(url) = hb_url.as_deref() {
            match client.post(url).json(&payload).send().await {
                Ok(_) => last_heartbeat_ts.store(now_ms(), Ordering::Relaxed),
                Err(e) => warn!(?e, "heartbeat falló"),
            }
        } else {
            info!(queue_len, "heartbeat local (sin URL configurada)");
            last_heartbeat_ts.store(now_ms(), Ordering::Relaxed);
        }
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[derive(Serialize)]
struct EventsBatch<'a> {
    device_id: &'a str,
    agent_version: &'a str,
    events: Vec<PayloadItem>,
}

#[derive(Serialize)]
struct PayloadItem {
    id: i64,
    payload_b64: String,
}

pub async fn run_sender_loop(state: Arc<AgentState>, paths: &Paths) {
    let client = Client::builder().build().expect("client http");
    let url = match std::env::var("EVENTS_URL") { Ok(u) => u, Err(_) => { info!("EVENTS_URL no configurado; skip sender"); return; } };
    let mut backoff = 1u64;
    loop {
        // pequeña pausa base
        sleep(Duration::from_secs(5)).await;
        let q = match agent_core::queue::Queue::open(paths, &state) { Ok(q) => q, Err(_) => continue };
        let batch = match q.fetch_batch(100) { Ok(b) => b, Err(_) => vec![] };
        if batch.is_empty() { backoff = 1; continue; }
        let items: Vec<PayloadItem> = batch.iter().map(|(id, blob)| PayloadItem { id: *id, payload_b64: base64::engine::general_purpose::STANDARD.encode(blob) }).collect();
        let body = EventsBatch { device_id: &state.device_id, agent_version: &state.agent_version, events: items };
        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                let ids: Vec<i64> = batch.iter().map(|(id,_)| *id).collect();
                if let Ok(count) = q.delete_ids(&ids) { info!(count, "eventos enviados y eliminados de la cola"); }
                backoff = 1;
            }
            Ok(resp) => { warn!(status=?resp.status(), "envío de eventos falló"); sleep(Duration::from_secs(backoff)).await; backoff = (backoff*2).min(60); }
            Err(e) => { warn!(?e, "error de red al enviar eventos"); sleep(Duration::from_secs(backoff)).await; backoff = (backoff*2).min(60); }
        }
    }
}
