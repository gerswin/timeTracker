use anyhow::Result;
use directories::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};

const QUALIFIER: &str = "com";
const ORGANIZATION: &str = "Ripor";
const APPLICATION: &str = "RiporAgent";

#[derive(Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let proj = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
            .ok_or_else(|| anyhow::anyhow!("No se pudo determinar ProjectDirs"))?;
        let data_dir = proj.data_dir().to_path_buf();
        if !data_dir.exists() {
            fs::create_dir_all(&data_dir)?;
        }
        Ok(Self { data_dir })
    }

    pub fn queue_db(&self) -> PathBuf {
        self.data_dir.join("queue.sqlite")
    }

    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("agent_state.json")
    }

    pub fn key_file(&self) -> PathBuf {
        self.data_dir.join("key.bin")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn secrets_file(&self) -> PathBuf {
        self.data_dir.join("agent_secrets.json")
    }

    pub fn policy_file(&self) -> PathBuf {
        self.data_dir.join("policy.json")
    }

    pub fn policy_meta_file(&self) -> PathBuf {
        self.data_dir.join("policy_meta.json")
    }
}

pub fn ensure_parent(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}
