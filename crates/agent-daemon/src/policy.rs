use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Policy {
    #[serde(default)]
    pub killSwitch: bool,
    #[serde(default)]
    pub pauseCapture: bool,
    #[serde(default = "default_true")]
    pub titleCapture: bool,
    #[serde(default)]
    pub excludeApps: Vec<String>,
    #[serde(default)]
    pub excludePatterns: Vec<String>,
    #[serde(default)]
    pub excludeExePaths: Vec<String>,
    #[serde(default)]
    pub updateChannel: Option<String>,
    #[serde(default)]
    pub titleSampleHz: Option<u32>,
    #[serde(default)]
    pub titleBurstPerMinute: Option<u32>,
    #[serde(default)]
    pub focusMinMinutes: Option<u32>,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicyMeta { pub etag: Option<String> }

#[derive(Debug, Default)]
pub struct PolicyRuntime { inner: RwLock<PolicyState> }

#[derive(Debug, Default, Clone)]
pub struct PolicyState { pub policy: Policy, pub etag: Option<String> }

impl PolicyRuntime {
    pub fn new() -> Arc<Self> { Arc::new(Self { inner: RwLock::new(PolicyState::default()) }) }
    pub fn get(&self) -> PolicyState { self.inner.read().unwrap().clone() }
    pub fn set(&self, st: PolicyState) { *self.inner.write().unwrap() = st; }
}

pub fn load_policy(paths: &agent_core::paths::Paths) -> PolicyState {
    let mut st = PolicyState::default();
    let pf = paths.policy_file();
    if pf.exists() {
        if let Ok(txt) = std::fs::read_to_string(&pf) { if let Ok(p) = serde_json::from_str::<Policy>(&txt) { st.policy = p; } }
    }
    let mf = paths.policy_meta_file();
    if mf.exists() {
        if let Ok(txt) = std::fs::read_to_string(&mf) { if let Ok(m) = serde_json::from_str::<PolicyMeta>(&txt) { st.etag = m.etag; } }
    }
    st
}

pub fn save_policy(paths: &agent_core::paths::Paths, st: &PolicyState) -> Result<()> {
    let pf = paths.policy_file();
    if let Some(dir) = pf.parent() { std::fs::create_dir_all(dir).ok(); }
    std::fs::write(&pf, serde_json::to_vec_pretty(&st.policy)?)?;
    let mf = paths.policy_meta_file();
    std::fs::write(&mf, serde_json::to_vec_pretty(&PolicyMeta { etag: st.etag.clone() })?)?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct DropCounters {
    pub kill_switch: std::sync::atomic::AtomicU64,
    pub pause: std::sync::atomic::AtomicU64,
    pub excluded_app: std::sync::atomic::AtomicU64,
    pub excluded_pattern: std::sync::atomic::AtomicU64,
    pub throttled: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DropEvent {
    pub ts_ms: u64,
    pub reason: String,
    pub app: String,
    pub title: String,
}

#[derive(Debug)]
pub struct DropLog { inner: Mutex<VecDeque<DropEvent>>, cap: usize }

impl DropLog {
    pub fn new(cap: usize) -> Arc<Self> { Arc::new(Self { inner: Mutex::new(VecDeque::with_capacity(cap.min(10_000))), cap: cap.max(1) }) }
    pub fn push(&self, ev: DropEvent) {
        let mut q = self.inner.lock().unwrap();
        if q.len() >= self.cap { q.pop_front(); }
        q.push_back(ev);
    }
    pub fn list_desc(&self, limit: usize) -> Vec<DropEvent> {
        let q = self.inner.lock().unwrap();
        let n = q.len();
        let take = limit.min(n);
        q.iter().rev().take(take).cloned().collect()
    }
}
