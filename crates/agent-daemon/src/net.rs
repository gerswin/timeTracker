use agent_core::auth::AgentSecrets;
use agent_core::metrics::MetricsHandle;
use agent_core::paths::Paths;
use agent_core::state::AgentState;
use reqwest::Client;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn, debug};
use hmac::{Hmac, Mac};
use sha2::Sha256;

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
    let api_base = std::env::var("API_BASE_URL").ok();
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
        let _m = metrics.get();
        if let Some(base) = api_base.as_deref() {
            if let Some(secrets) = AgentSecrets::load(paths).ok().flatten() {
                let body = serde_json::json!({
                    "status": "running",
                    "uptime_seconds": 0,
                    "last_activity_ms": last_evt,
                    "agent_version": state.agent_version,
                });
                let body_str = serde_json::to_string(&body).unwrap();
                let sig = hmac_hex(&secrets.server_salt, body_str.as_bytes());
                let url = format!("{}/v1/agents/heartbeat", base.trim_end_matches('/'));
                if std::env::var("RIPOR_DEBUG_INGEST").ok().as_deref() == Some("1") {
                    debug!(payload=%body_str, url=%url, "heartbeat payload");
                }
                match client.post(url)
                    .header("Content-Type", "application/json")
                    .header("Agent-Token", secrets.agent_token)
                    .header("X-Body-HMAC", sig)
                    .body(body_str)
                    .send().await {
                    Ok(resp) if resp.status().is_success() => {
                        last_heartbeat_ts.store(now_ms(), Ordering::Relaxed);
                    }
                    Ok(resp) => warn!(status=?resp.status(), "heartbeat falló"),
                    Err(e) => warn!(?e, "heartbeat error red"),
                }
                continue;
            }
        }
        info!(queue_len, "heartbeat local (sin API_BASE_URL o sin bootstrap)");
        last_heartbeat_ts.store(now_ms(), Ordering::Relaxed);
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub async fn run_sender_loop(state: Arc<AgentState>, paths: &Paths) {
    let client = Client::builder().build().expect("client http");
    let api_base = match std::env::var("API_BASE_URL") { Ok(u) => u, Err(_) => { info!("API_BASE_URL no configurado; skip sender"); return; } };
    let mut backoff = 1u64;
    loop {
        // pequeña pausa base
        sleep(Duration::from_secs(5)).await;
        let q = match agent_core::queue::Queue::open(paths, &state) {
            Ok(q) => q,
            Err(_) => continue,
        };
        let batch = match q.fetch_batch(100) {
            Ok(b) => b,
            Err(_) => vec![],
        };
        if batch.is_empty() {
            backoff = 1;
            continue;
        }
        // Require secrets for authenticated ingest
        let secrets = match AgentSecrets::load(paths).ok().flatten() { Some(s) => s, None => { info!("sin bootstrap; skip ingest"); continue; } };
        let mac = get_primary_mac().unwrap_or_default();
        let os_name = std::env::consts::OS;
        let mut events_json: Vec<serde_json::Value> = Vec::new();
        for (_id, blob) in &batch {
            if let Ok(evt) = serde_json::from_slice::<serde_json::Value>(blob) {
                let app = evt.get("app_name").and_then(|v| v.as_str()).unwrap_or_default();
                let title = evt.get("window_title").and_then(|v| v.as_str()).unwrap_or_default();
                let ts = evt.get("ts_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let idle = evt.get("input_idle_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let state_s = if idle < idle_threshold_ms() { "active" } else { "idle" };
                events_json.push(serde_json::json!({
                    "org_id": std::env::var("ORG_ID").ok().unwrap_or_default(),
                    "user_email": std::env::var("USER_EMAIL").ok().unwrap_or_default(),
                    "device_id": secrets.device_id.as_deref().unwrap_or(&state.device_id),
                    "mac_address": mac,
                    "os": os_name,
                    "app_name": app,
                    "window_title": title,
                    "state": state_s,
                    "timestamp_ms": ts,
                    "dur_ms": 0,
                    "category": "",
                    "focus": true,
                    "focus_start_ms": ts,
                    "focus_end_ms": ts,
                    "input_idle_ms": idle,
                    "media_hint": "",
                    "agent_version": state.agent_version,
                }));
            }
        }
        let body = serde_json::json!({ "events": events_json });
        let body_str = serde_json::to_string(&body).unwrap();
        let sig = hmac_hex(&secrets.server_salt, body_str.as_bytes());
        let url = format!("{}/v1/events:ingest", api_base.trim_end_matches('/'));
        if std::env::var("RIPOR_DEBUG_INGEST").ok().as_deref() == Some("1") {
            debug!(payload=%body_str, url=%url, count=%events_json.len(), "ingest payload");
        }
        match client.post(url)
            .header("Content-Type", "application/json")
            .header("Agent-Token", secrets.agent_token)
            .header("X-Body-HMAC", sig)
            .body(body_str)
            .send().await {
            Ok(resp) if resp.status().is_success() => {
                let ids: Vec<i64> = batch.iter().map(|(id, _)| *id).collect();
                if let Ok(count) = q.delete_ids(&ids) {
                    info!(count, "eventos enviados y eliminados de la cola");
                }
                backoff = 1;
            }
            Ok(resp) => {
                warn!(status=?resp.status(), "envío de eventos falló");
                sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
            Err(e) => {
                warn!(?e, "error de red al enviar eventos");
                sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

type HmacSha256 = Hmac<Sha256>;
fn hmac_hex(key: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("hmac key");
    mac.update(body);
    let res = mac.finalize().into_bytes();
    hex::encode(res)
}

fn idle_threshold_ms() -> u64 {
    std::env::var("IDLE_ACTIVE_THRESHOLD_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60_000)
}

fn get_primary_mac() -> Option<String> {
    match mac_address::get_mac_address() {
        Ok(Some(ma)) => Some(format!("{}", ma)),
        _ => None,
    }
}

pub async fn bootstrap_if_needed(paths: &Paths, state: &AgentState) {
    if AgentSecrets::load(paths).ok().flatten().is_some() { return; }
    let base = match std::env::var("API_BASE_URL") { Ok(u) => u, Err(_) => { info!("API_BASE_URL no configurado; skip bootstrap"); return; } };
    let org = match std::env::var("ORG_ID") { Ok(v) if !v.is_empty() => v, _ => { info!("ORG_ID no configurado; skip bootstrap"); return; } };
    let user = match std::env::var("USER_EMAIL") { Ok(v) if !v.is_empty() => v, _ => { info!("USER_EMAIL no configurado; skip bootstrap"); return; } };
    let mac = get_primary_mac().unwrap_or_default();
    let body = serde_json::json!({
        "org_id": org,
        "user_email": user,
        "mac_address": mac,
        "agent_version": state.agent_version,
    });
    let url = format!("{}/v1/agents/bootstrap", base.trim_end_matches('/'));
    let client = Client::builder().build().expect("client http");
    match client.post(url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<serde_json::Value>().await {
                Ok(v) => {
                    let token = v.get("agentToken").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let salt = v.get("serverSalt").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let dev = v.get("deviceId").and_then(|x| x.as_str()).map(|s| s.to_string());
                    if !token.is_empty() && !salt.is_empty() {
                        let secrets = AgentSecrets { agent_token: token, server_salt: salt, device_id: dev };
                        if let Err(e) = secrets.save(paths) { warn!(?e, "no se pudo guardar secrets"); }
                        else { info!("bootstrap ok: token guardado"); }
                    } else {
                        warn!(?v, "bootstrap respuesta incompleta");
                    }
                }
                Err(e) => warn!(?e, "bootstrap parse fallo"),
            }
        }
        Ok(resp) => warn!(status=?resp.status(), "bootstrap falló"),
        Err(e) => warn!(?e, "bootstrap error red"),
    }
}
