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
use crate::policy::{PolicyRuntime, PolicyState, load_policy, save_policy};

#[derive(Serialize)]
struct HeartbeatPayload<'a> {
    device_id: &'a str,
    agent_version: &'a str,
    last_event_ts: u64,
    queue_len: i64,
    cpu_pct: f32,
    mem_mb: u64,
}

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

/// Valida el esquema de API_BASE_URL: https siempre OK; http solo para
/// loopback (localhost, 127.0.0.1, [::1]) o si RIPOR_ALLOW_HTTP=1 (dev).
fn validate_api_base(url: &str, allow_http: bool) -> Option<String> {
    let trimmed = url.trim();
    if let Some(rest) = trimmed.strip_prefix("https://") {
        if rest.is_empty() {
            return None;
        }
        return Some(trimmed.to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        if rest.is_empty() {
            return None;
        }
        if allow_http || is_loopback_host(rest) {
            return Some(trimmed.to_string());
        }
        return None;
    }
    None
}

/// Extrae el host de la porción tras "http://" y determina si es loopback:
/// localhost, 127.0.0.1 o [::1] (con o sin puerto/ruta, case-insensitive).
/// Descarta el userinfo (todo hasta el último '@' de la autoridad) para que
/// "http://localhost@evil.com/" no cuele evil.com como loopback.
fn is_loopback_host(after_scheme: &str) -> bool {
    // autoridad = hasta el primer terminador de autoridad (/, \, ?, #) — WHATWG special-scheme
    let authority = after_scheme
        .split(|c| c == '/' || c == '\\' || c == '?' || c == '#')
        .next()
        .unwrap_or("");
    // descartar userinfo: el host real es lo que sigue al último '@'
    let hostport = authority.rsplit('@').next().unwrap_or("");
    // separar host de :puerto (soporta [::1] y [::1]:port)
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// Lee API_BASE_URL (+ RIPOR_ALLOW_HTTP) del entorno y valida el esquema.
/// Una URL rechazada se comporta como no configurada (None), con un warn!
/// explicando el motivo.
pub(crate) fn api_base_url() -> Option<String> {
    let raw = std::env::var("API_BASE_URL").ok()?;
    let allow_http = std::env::var("RIPOR_ALLOW_HTTP").ok().as_deref() == Some("1");
    match validate_api_base(&raw, allow_http) {
        Some(u) => Some(u),
        None => {
            warn!(
                url = %raw,
                "API_BASE_URL rechazado: se requiere https (http solo permitido para loopback o con RIPOR_ALLOW_HTTP=1)"
            );
            None
        }
    }
}

pub async fn run_heartbeat_loop(
    state: Arc<AgentState>,
    paths: &Paths,
    metrics: MetricsHandle,
    last_event_ts: Arc<AtomicU64>,
    last_heartbeat_ts: Arc<AtomicU64>,
    key: Option<[u8; 32]>,
) {
    info!("iniciando loop de heartbeat (Fase 1)");
    let client = Client::builder().build().expect("client http");
    let api_base = api_base_url();
    let gc_cfg = gc_config_from_env();
    loop {
        sleep(Duration::from_secs(60)).await;
        let last_evt = last_event_ts.load(Ordering::Relaxed);
        // Cola no disponible (sin clave cargada) o error al abrirla/consultarla:
        // -1 ("desconocido"), nunca 0 (0 se confunde con "cola vacía").
        let queue_len = match key {
            None => -1,
            Some(k) => match agent_core::queue::Queue::open_with_key(paths, &state, k) {
                Ok(q) => {
                    match q.gc(&gc_cfg) {
                        Ok(stats) => {
                            if stats.pruned_age > 0 || stats.pruned_attempts > 0 || stats.pruned_overflow > 0 {
                                info!(
                                    pruned_age = stats.pruned_age,
                                    pruned_attempts = stats.pruned_attempts,
                                    pruned_overflow = stats.pruned_overflow,
                                    "gc de cola: filas eliminadas"
                                );
                            }
                        }
                        Err(e) => warn!(?e, "gc de cola falló"),
                    }
                    q.queue_len().unwrap_or(-1)
                }
                Err(_) => -1,
            },
        };
        let m = metrics.get();
        if let Some(base) = api_base.as_deref() {
            if let Some(secrets) = AgentSecrets::load(paths).ok().flatten() {
                let body_str = heartbeat_body(
                    secrets.device_id.as_deref().unwrap_or(&state.device_id),
                    &state.agent_version,
                    last_evt,
                    queue_len,
                    m.cpu_pct,
                    m.mem_mb,
                );
                let sig = hmac_hex(&secrets.server_salt, body_str.as_bytes());
                let url = format!("{}/v1/agents/heartbeat", base.trim_end_matches('/'));
                if std::env::var("RIPOR_DEBUG_INGEST").ok().as_deref() == Some("1") {
                    debug!(payload=%body_str, url=%url, "heartbeat payload");
                }
                match client.post(url.clone())
                    .header("Content-Type", "application/json")
                    .header("Agent-Token", secrets.agent_token.clone())
                    .header("X-Body-HMAC", sig.clone())
                    .body(body_str.clone())
                    .send().await {
                    Ok(resp) if resp.status().is_success() => { last_heartbeat_ts.store(now_ms(), Ordering::Relaxed); }
                    Ok(resp) if resp.status().as_u16() == 401 => {
                        if let Some(newsec) = rebootstrap(paths, &state).await {
                            let sig2 = hmac_hex(&newsec.server_salt, body_str.as_bytes());
                            match client.post(url)
                                .header("Content-Type", "application/json")
                                .header("Agent-Token", newsec.agent_token)
                                .header("X-Body-HMAC", sig2)
                                .body(body_str)
                                .send().await {
                                Ok(r2) if r2.status().is_success() => { last_heartbeat_ts.store(now_ms(), Ordering::Relaxed); }
                                Ok(r2) => warn!(status=?r2.status(), "heartbeat tras re-bootstrap falló"),
                                Err(e2) => warn!(?e2, "heartbeat error red tras re-bootstrap"),
                            }
                        } else { warn!("re-bootstrap no disponible"); }
                    }
                    Ok(resp) if resp.status().as_u16() == 403 => { warn!(status=?resp.status(), "heartbeat forbidden (403)"); }
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

pub async fn run_sender_loop(state: Arc<AgentState>, paths: &Paths, key: [u8; 32]) {
    let client = Client::builder().build().expect("client http");
    let api_base = match api_base_url() { Some(u) => u, None => { info!("API_BASE_URL no configurado o inválido; skip sender"); return; } };
    let mut backoff = 1u64;
    loop {
        // pequeña pausa base
        sleep(Duration::from_secs(5)).await;
        let q = match agent_core::queue::Queue::open_with_key(paths, &state, key) {
            Ok(q) => q,
            Err(_) => continue,
        };
        let batch = match q.fetch_batch_decrypted(100) {
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
        for (_id, plain) in &batch {
            if let Ok(evt) = serde_json::from_slice::<serde_json::Value>(plain) {
                let app = evt.get("app_name").and_then(|v| v.as_str()).unwrap_or_default();
                let title = evt.get("window_title").and_then(|v| v.as_str()).unwrap_or_default();
                let org = std::env::var("ORG_ID").ok().unwrap_or_default();
                let user = std::env::var("USER_EMAIL").ok().unwrap_or_default();
                if evt.get("type").and_then(|v| v.as_str()) == Some("focus_block") {
                    let fs = evt.get("focus_start_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let fe = evt.get("focus_end_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let dur = evt.get("dur_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let ts = fe.max(fs);
                    events_json.push(serde_json::json!({
                        "org_id": org,
                        "user_email": user,
                        "device_id": secrets.device_id.as_deref().unwrap_or(&state.device_id),
                        "mac_address": mac,
                        "os": os_name,
                        "app_name": app,
                        "window_title": title,
                        "state": "active",
                        "timestamp_ms": ts,
                        "dur_ms": dur,
                        "category": "",
                        "focus": true,
                        "focus_start_ms": fs,
                        "focus_end_ms": fe,
                        "input_idle_ms": 0,
                        "media_hint": "",
                        "agent_version": state.agent_version,
                    }));
                } else {
                    let ts = evt.get("ts_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let idle = evt.get("input_idle_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let state_s = if idle < idle_threshold_ms() { "active" } else { "idle" };
                    events_json.push(serde_json::json!({
                        "org_id": org,
                        "user_email": user,
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
        }
        let body = serde_json::json!({ "events": events_json });
        let body_str = serde_json::to_string(&body).unwrap();
        let sig = hmac_hex(&secrets.server_salt, body_str.as_bytes());
        let url = format!("{}/v1/events:ingest", api_base.trim_end_matches('/'));
        if std::env::var("RIPOR_DEBUG_INGEST").ok().as_deref() == Some("1") {
            if std::env::var("RIPOR_DEBUG").ok().as_deref() == Some("1") {
                debug!(payload=%body_str, url=%url, count=%events_json.len(), "ingest payload");
            } else {
                debug!(url=%url, count=%events_json.len(), "ingest payload (body omitido; títulos completos solo con RIPOR_DEBUG=1)");
            }
        }
        let ids: Vec<i64> = batch.iter().map(|(id, _)| *id).collect();
        let mut ok_sent = false;
        match client.post(url.clone())
            .header("Content-Type", "application/json")
            .header("Agent-Token", secrets.agent_token.clone())
            .header("X-Body-HMAC", sig.clone())
            .body(body_str.clone())
            .send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(count) = q.delete_ids(&ids) {
                    info!(count, "eventos enviados y eliminados de la cola");
                }
                backoff = 1;
                ok_sent = true;
            }
            Ok(resp) if resp.status().as_u16() == 401 => {
                if let Some(newsec) = rebootstrap(paths, &state).await {
                    let sig2 = hmac_hex(&newsec.server_salt, body_str.as_bytes());
                    match client.post(url)
                        .header("Content-Type", "application/json")
                        .header("Agent-Token", newsec.agent_token)
                        .header("X-Body-HMAC", sig2)
                        .body(body_str)
                        .send().await {
                        Ok(r2) if r2.status().is_success() => {
                            if let Ok(count) = q.delete_ids(&ids) { info!(count, "eventos enviados tras re-bootstrap y eliminados"); }
                            backoff = 1; ok_sent = true;
                        }
                        Ok(r2) => {
                            warn!(status=?r2.status(), "ingest tras re-bootstrap falló");
                            sleep(Duration::from_secs(backoff)).await;
                            backoff = (backoff * 2).min(60);
                        }
                        Err(e2) => {
                            warn!(?e2, "ingest error red tras re-bootstrap");
                            sleep(Duration::from_secs(backoff)).await;
                            backoff = (backoff * 2).min(60);
                        }
                    }
                } else {
                    warn!("re-bootstrap no disponible");
                    sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(60);
                }
            }
            Ok(resp) if resp.status().as_u16() == 403 => {
                warn!(status=?resp.status(), "ingest forbidden (403)");
                sleep(Duration::from_secs(backoff)).await; backoff = (backoff*2).min(60);
            }
            Ok(resp) if is_payload_rejection(resp.status().as_u16()) => {
                // El servidor rechaza el payload en sí: reintentar es inútil.
                // Solo aquí se cuenta el intento, para que el GC por attempts
                // pode el batch venenoso; los fallos de red/auth/servidor
                // reintentan sin límite (el tope offline es el GC por edad).
                warn!(status=?resp.status(), "ingest rechazó el payload; se incrementa attempts");
                if let Err(ie) = q.increment_attempts(&ids) { warn!(?ie, "no se pudo incrementar attempts"); }
                sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
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
        if ok_sent { continue; }
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
    let base = match api_base_url() { Some(u) => u, None => { info!("API_BASE_URL no configurado o inválido; skip bootstrap"); return; } };
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

async fn rebootstrap(paths: &Paths, state: &AgentState) -> Option<AgentSecrets> {
    let base = match api_base_url() { Some(u) => u, None => { info!("API_BASE_URL no configurado o inválido; no se puede re-bootstrap"); return None; } };
    let org = match std::env::var("ORG_ID") { Ok(v) if !v.is_empty() => v, _ => { info!("ORG_ID no configurado; no se puede re-bootstrap"); return None; } };
    let user = match std::env::var("USER_EMAIL") { Ok(v) if !v.is_empty() => v, _ => { info!("USER_EMAIL no configurado; no se puede re-bootstrap"); return None; } };
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
                        if let Err(e) = secrets.save(paths) { warn!(?e, "no se pudo guardar secrets"); None }
                        else { info!("re-bootstrap ok: token actualizado"); Some(secrets) }
                    } else { warn!(?v, "re-bootstrap respuesta incompleta"); None }
                }
                Err(e) => { warn!(?e, "re-bootstrap parse fallo"); None }
            }
        }
        Ok(resp) => { warn!(status=?resp.status(), "re-bootstrap falló"); None }
        Err(e) => { warn!(?e, "re-bootstrap error red"); None }
    }
}

fn poll_secs_from(v: Option<&str>) -> u64 {
    v.and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(10)
}

fn gc_config_from(
    age_days: Option<&str>,
    rows: Option<&str>,
    attempts: Option<&str>,
) -> agent_core::queue::GcConfig {
    let max_age_days = age_days
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(14);
    let max_rows = rows
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(100_000);
    let max_attempts = attempts
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(50);
    agent_core::queue::GcConfig {
        max_age_ms: max_age_days * 24 * 60 * 60 * 1000,
        max_rows,
        max_attempts,
    }
}

/// Estados HTTP que indican que el servidor rechaza el payload en sí
/// (reintentar el mismo batch es inútil). Solo estos cuentan attempts.
fn is_payload_rejection(status: u16) -> bool {
    matches!(status, 400 | 413 | 422)
}

fn gc_config_from_env() -> agent_core::queue::GcConfig {
    gc_config_from(
        std::env::var("QUEUE_MAX_AGE_DAYS").ok().as_deref(),
        std::env::var("QUEUE_MAX_ROWS").ok().as_deref(),
        std::env::var("QUEUE_MAX_ATTEMPTS").ok().as_deref(),
    )
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

pub async fn run_policy_loop(paths: &Paths, rt: Arc<PolicyRuntime>) {
    // load initial from disk
    let initial = load_policy(paths);
    rt.set(initial.clone());
    let client = Client::builder().build().expect("client http");
    loop {
        let base = match api_base_url() { Some(u) => u, None => { sleep(Duration::from_secs(60)).await; continue; } };
        let user = match std::env::var("USER_EMAIL") { Ok(v) if !v.is_empty() => v, _ => { sleep(Duration::from_secs(60)).await; continue; } };
        // secrets required
        let secrets = match AgentSecrets::load(paths).ok().flatten() { Some(s) => s, None => { sleep(Duration::from_secs(30)).await; continue; } };
        let url = format!("{}/v1/policy/{}", base.trim_end_matches('/'), urlencoding::encode(&user));
        let etag = rt.get().etag;
        let mut req = client.get(url).header("Agent-Token", secrets.agent_token);
        if let Some(tag) = etag.as_deref() { req = req.header("If-None-Match", tag); }
        match req.send().await {
            Ok(resp) if resp.status().as_u16() == 304 => { /* unchanged */ }
            Ok(resp) if resp.status().is_success() => {
                apply_policy_response(resp, paths, &rt).await;
            }
            Ok(resp) if resp.status().as_u16() == 401 => {
                // try rebootstrap then retry once
                if let Some(ns) = rebootstrap(paths, &AgentState { device_id: String::new(), agent_version: String::new(), created_at: 0, updated_at: 0 }).await {
                    let url2 = format!("{}/v1/policy/{}", base.trim_end_matches('/'), urlencoding::encode(&user));
                    let mut r2 = client.get(url2).header("Agent-Token", ns.agent_token);
                    if let Some(tag) = etag.as_deref() { r2 = r2.header("If-None-Match", tag); }
                    match r2.send().await {
                        Ok(r2resp) if r2resp.status().as_u16() == 304 => { /* sin cambios */ }
                        Ok(r2resp) if r2resp.status().is_success() => {
                            apply_policy_response(r2resp, paths, &rt).await;
                        }
                        Ok(r2resp) => warn!(status=?r2resp.status(), "policy tras re-bootstrap falló"),
                        Err(e2) => warn!(?e2, "policy error red tras re-bootstrap"),
                    }
                }
            }
            Ok(resp) => warn!(status=?resp.status(), "policy fallo"),
            Err(e) => warn!(?e, "policy error red"),
        }
        sleep(Duration::from_secs(policy_poll_secs())).await;
    }
}


pub async fn fetch_policy_once(paths: &Paths, rt: std::sync::Arc<PolicyRuntime>) {
    let client = Client::builder().build().expect("client http");
    let base = match api_base_url() { Some(u) => u, None => return };
    let user = match std::env::var("USER_EMAIL") { Ok(v) if !v.is_empty() => v, _ => return };
    let secrets = match AgentSecrets::load(paths).ok().flatten() { Some(s) => s, None => return };
    let url = format!("{}/v1/policy/{}", base.trim_end_matches('/'), urlencoding::encode(&user));
    let etag = rt.get().etag;
    let mut req = client.get(url).header("Agent-Token", secrets.agent_token);
    if let Some(tag) = etag.as_deref() { req = req.header("If-None-Match", tag); }
    if let Ok(resp) = req.send().await {
        if resp.status().is_success() {
            let hdr = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                let polv = v.get("policy").cloned().unwrap_or(v);
                if let Ok(policy) = serde_json::from_value::<crate::policy::Policy>(polv) {
                    let st = PolicyState { policy, etag: hdr };
                    let _ = save_policy(paths, &st);
                    rt.set(st);
                }
            }
        }
    }
}

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

    #[test]
    fn gc_config_from_defaults() {
        let cfg = gc_config_from(None, None, None);
        assert_eq!(cfg.max_age_ms, 14 * 24 * 60 * 60 * 1000);
        assert_eq!(cfg.max_rows, 100_000);
        assert_eq!(cfg.max_attempts, 50);
    }

    #[test]
    fn gc_config_from_lee_valores_validos() {
        let cfg = gc_config_from(Some("7"), Some("500"), Some("10"));
        assert_eq!(cfg.max_age_ms, 7 * 24 * 60 * 60 * 1000);
        assert_eq!(cfg.max_rows, 500);
        assert_eq!(cfg.max_attempts, 10);
    }

    #[test]
    fn gc_config_from_rechaza_invalidos() {
        let cfg = gc_config_from(Some("abc"), Some("0"), Some("-1"));
        assert_eq!(cfg.max_age_ms, 14 * 24 * 60 * 60 * 1000);
        assert_eq!(cfg.max_rows, 100_000);
        assert_eq!(cfg.max_attempts, 50);
    }

    #[test]
    fn validate_api_base_https_ok() {
        assert_eq!(
            validate_api_base("https://api.example.com", false),
            Some("https://api.example.com".to_string())
        );
    }

    #[test]
    fn validate_api_base_https_remoto_con_puerto_ok() {
        assert_eq!(
            validate_api_base("https://api.example.com:8443", false),
            Some("https://api.example.com:8443".to_string())
        );
    }

    #[test]
    fn validate_api_base_recorta_espacios() {
        assert_eq!(
            validate_api_base("  https://api.example.com  ", false),
            Some("https://api.example.com".to_string())
        );
    }

    #[test]
    fn validate_api_base_http_localhost_ok() {
        assert_eq!(
            validate_api_base("http://localhost", false),
            Some("http://localhost".to_string())
        );
        assert_eq!(
            validate_api_base("http://localhost:8787", false),
            Some("http://localhost:8787".to_string())
        );
    }

    #[test]
    fn validate_api_base_http_127_0_0_1_ok() {
        assert_eq!(
            validate_api_base("http://127.0.0.1:49219", false),
            Some("http://127.0.0.1:49219".to_string())
        );
    }

    #[test]
    fn validate_api_base_http_ipv6_loopback_ok() {
        assert_eq!(
            validate_api_base("http://[::1]:9000", false),
            Some("http://[::1]:9000".to_string())
        );
        assert_eq!(
            validate_api_base("http://[::1]", false),
            Some("http://[::1]".to_string())
        );
    }

    #[test]
    fn validate_api_base_http_remoto_rechazado() {
        assert_eq!(validate_api_base("http://evil.example.com", false), None);
    }

    #[test]
    fn validate_api_base_http_remoto_con_allow_http_ok() {
        assert_eq!(
            validate_api_base("http://evil.example.com", true),
            Some("http://evil.example.com".to_string())
        );
    }

    #[test]
    fn validate_api_base_esquema_invalido_o_ausente() {
        assert_eq!(validate_api_base("ftp://example.com", false), None);
        assert_eq!(validate_api_base("garbage", false), None);
        assert_eq!(validate_api_base("", false), None);
    }

    #[test]
    fn validate_api_base_userinfo_no_cuela_host_remoto_como_loopback() {
        // userinfo (parte antes de '@') no debe colar un host remoto como loopback
        assert_eq!(validate_api_base("http://localhost:1234@evil.com/", false), None);
        assert_eq!(validate_api_base("http://[::1]@evil.com/", false), None);
        assert_eq!(validate_api_base("http://[::1]:9@evil.com/", false), None);
        assert_eq!(validate_api_base("http://127.0.0.1@evil.com/", false), None);
        // el host real es evil.com/@localhost -> evil.com (autoridad termina en '/')
        assert_eq!(validate_api_base("http://evil.com/@localhost/", false), None);
    }

    #[test]
    fn validate_api_base_terminadores_whatwg_no_cuelan_host_remoto() {
        // La autoridad termina en el PRIMER de / \ ? # (WHATWG special-scheme):
        // en estos casos el host real es evil.com, no localhost.
        assert_eq!(validate_api_base("http://evil.com\\@localhost/", false), None);
        assert_eq!(validate_api_base("http://evil.com#@localhost/", false), None);
        assert_eq!(validate_api_base("http://evil.com?@localhost/", false), None);
    }

    #[test]
    fn validate_api_base_userinfo_con_host_loopback_legitimo_ok() {
        // loopback legítimo con userinfo sí es loopback (el host real es loopback)
        assert_eq!(
            validate_api_base("http://user@localhost:8080/", false).as_deref(),
            Some("http://user@localhost:8080/")
        );
    }

    #[test]
    fn validate_api_base_loopback_case_insensitive() {
        assert_eq!(
            validate_api_base("http://LOCALHOST/", false).as_deref(),
            Some("http://LOCALHOST/")
        );
    }

    #[test]
    fn payload_rejection_solo_400_413_422() {
        assert!(is_payload_rejection(400));
        assert!(is_payload_rejection(413));
        assert!(is_payload_rejection(422));
        // transporte/auth/servidor: reintentar es correcto, no cuentan attempts
        assert!(!is_payload_rejection(401));
        assert!(!is_payload_rejection(403));
        assert!(!is_payload_rejection(429));
        assert!(!is_payload_rejection(500));
        assert!(!is_payload_rejection(503));
    }
}
