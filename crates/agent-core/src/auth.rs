use crate::paths::Paths;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentSecrets {
    pub agent_token: String,
    pub server_salt: String,
    pub device_id: Option<String>,
}

impl AgentSecrets {
    pub fn load(paths: &Paths) -> Result<Option<Self>> {
        let f = paths.secrets_file();
        if !f.exists() { return Ok(None); }
        let data = fs::read_to_string(&f)?;
        let s: AgentSecrets = serde_json::from_str(&data)?;
        Ok(Some(s))
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let f = paths.secrets_file();
        if let Some(p) = f.parent() { if !p.exists() { fs::create_dir_all(p)?; } }
        let body = serde_json::to_vec_pretty(self)?;
        fs::write(&f, body)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&f, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

