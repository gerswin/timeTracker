use crate::paths::{ensure_parent, Paths};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub device_id: String,
    pub agent_version: String,
    pub created_at: u64,
    pub updated_at: u64,
}

impl AgentState {
    pub fn load_or_init(paths: &Paths, agent_version: &str) -> Result<Self> {
        let f = paths.state_file();
        if f.exists() {
            let data = fs::read_to_string(&f)?;
            let mut st: AgentState = serde_json::from_str(&data)?;
            st.agent_version = agent_version.to_string();
            st.updated_at = now_ms();
            fs::write(&f, serde_json::to_vec_pretty(&st)?)?;
            return Ok(st);
        }
        let device_id = generate_device_id();
        let st = AgentState {
            device_id,
            agent_version: agent_version.to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        };
        ensure_parent(&f)?;
        fs::write(&f, serde_json::to_vec_pretty(&st)?)?;
        Ok(st)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn generate_device_id() -> String {
    // Para Fase 0: UUID v4 persistido en disco. En fases posteriores, considerar claves del SO.
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    // set version and variant per RFC 4122
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

