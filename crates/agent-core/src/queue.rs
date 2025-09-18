use crate::crypto::{decrypt_decompress, encrypt_compress, load_or_create_key};
use crate::paths::Paths;
use crate::state::AgentState;
use anyhow::Result;
use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Queue {
    conn: Connection,
    key: [u8; 32],
    aad: Vec<u8>,
}

impl Queue {
    pub fn open(paths: &Paths, state: &AgentState) -> Result<Self> {
        let key = load_or_create_key(paths)?;
        let conn = Connection::open(paths.queue_db())?;
        conn.pragma_update(None, "journal_mode", &"WAL")?;
        conn.pragma_update(None, "synchronous", &"NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                payload BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_events_created ON events(created_at);
            ",
        )?;
        Ok(Self {
            conn,
            key,
            aad: state.device_id.as_bytes().to_vec(),
        })
    }

    pub fn enqueue_json(&self, json_bytes: &[u8]) -> Result<i64> {
        let now = now_ms();
        let blob = encrypt_compress(&self.key, &self.aad, json_bytes)?;
        self.conn.execute(
            "INSERT INTO events(created_at, attempts, payload) VALUES (?1, 0, ?2)",
            params![now as i64, blob],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn queue_len(&self) -> Result<i64> {
        let mut stmt = self.conn.prepare("SELECT COUNT(1) FROM events")?;
        let cnt: i64 = stmt.query_row([], |row| row.get(0))?;
        Ok(cnt)
    }

    pub fn fetch_batch(&self, limit: usize) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, payload FROM events ORDER BY created_at ASC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for r in rows { out.push(r?); }
        Ok(out)
    }

    pub fn delete_ids(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() { return Ok(0); }
        let mut count = 0usize;
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = self.conn.prepare("DELETE FROM events WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    #[allow(dead_code)]
    pub fn peek_decrypted(&self, limit: usize) -> Result<Vec<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM events ORDER BY created_at ASC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let blob = r?;
            let plain = decrypt_decompress(&self.key, &self.aad, &blob)?;
            out.push(plain);
        }
        Ok(out)
    }

    pub fn peek_decrypted_desc(&self, limit: usize) -> Result<Vec<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM events ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let blob = r?;
            let plain = decrypt_decompress(&self.key, &self.aad, &blob)?;
            out.push(plain);
        }
        Ok(out)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
