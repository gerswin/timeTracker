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

/// Configuración de recolección de basura (GC) para la cola de eventos.
pub struct GcConfig {
    pub max_age_ms: u64,
    pub max_rows: u64,
    pub max_attempts: u32,
}

/// Conteo de filas eliminadas por cada categoría de GC.
#[derive(Debug, Default)]
pub struct GcStats {
    pub pruned_age: usize,
    pub pruned_attempts: usize,
    pub pruned_overflow: usize,
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
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
        CREATE INDEX IF NOT EXISTS idx_events_attempts ON events(attempts);
        ",
    )?;
    Ok(())
}

impl Queue {
    pub fn open(paths: &Paths, state: &AgentState) -> Result<Self> {
        let key = load_or_create_key(paths)?;
        let conn = Connection::open(paths.queue_db())?;
        init_schema(&conn)?;
        Ok(Self {
            conn,
            key,
            aad: state.device_id.as_bytes().to_vec(),
        })
    }

    /// Constructor solo para tests: evita tocar el keystore real del SO (Keychain).
    #[cfg(test)]
    pub(crate) fn open_with_key(paths: &Paths, state: &AgentState, key: [u8; 32]) -> Result<Self> {
        let conn = Connection::open(paths.queue_db())?;
        init_schema(&conn)?;
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

    /// Solo para tests: permite fijar `created_at` para probar GC por edad/overflow.
    #[cfg(test)]
    fn enqueue_json_at(&self, json: &[u8], created_at_ms: u64) -> Result<i64> {
        let blob = encrypt_compress(&self.key, &self.aad, json)?;
        self.conn.execute(
            "INSERT INTO events(created_at, attempts, payload) VALUES (?1, 0, ?2)",
            params![created_at_ms as i64, blob],
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

    /// Igual que `fetch_batch`, pero descifrado. Las filas que no se pueden
    /// descifrar (poison rows) se descartan y se borran de inmediato en vez
    /// de abortar todo el batch: nunca podrán enviarse, y dejarlas ahí
    /// bloquearía el envío del resto de la cola para siempre.
    pub fn fetch_batch_decrypted(&self, limit: usize) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut out = Vec::new();
        let mut poison_ids: Vec<i64> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, payload FROM events ORDER BY created_at ASC LIMIT ?1")?;
            let rows = stmt.query_map([limit as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            for r in rows {
                let (id, blob) = r?;
                match crate::crypto::decrypt_decompress(&self.key, &self.aad, &blob) {
                    Ok(plain) => out.push((id, plain)),
                    Err(_) => poison_ids.push(id),
                }
            }
        }
        if !poison_ids.is_empty() {
            tracing::warn!(
                count = poison_ids.len(),
                "descartando filas indescifrables de la cola (poison rows)"
            );
            self.delete_ids(&poison_ids)?;
        }
        Ok(out)
    }

    /// Marca un intento de envío fallido para estas filas.
    pub fn increment_attempts(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("UPDATE events SET attempts = attempts + 1 WHERE id IN ({placeholders})");
        let params: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        self.conn.execute(&sql, params.as_slice())?;
        Ok(())
    }

    /// Recolección de basura: poda por edad, por exceso de intentos, y por
    /// tope de filas (se conservan las `max_rows` más recientes).
    pub fn gc(&self, cfg: &GcConfig) -> Result<GcStats> {
        let cutoff = now_ms().saturating_sub(cfg.max_age_ms) as i64;
        let pruned_age = self
            .conn
            .execute("DELETE FROM events WHERE created_at < ?1", params![cutoff])?;
        let pruned_attempts = self.conn.execute(
            "DELETE FROM events WHERE attempts > ?1",
            params![cfg.max_attempts as i64],
        )?;
        let pruned_overflow = self.conn.execute(
            "DELETE FROM events WHERE id NOT IN (SELECT id FROM events ORDER BY created_at DESC LIMIT ?1)",
            params![cfg.max_rows as i64],
        )?;
        Ok(GcStats { pruned_age, pruned_attempts, pruned_overflow })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    fn tmp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { data_dir: dir.path().to_path_buf() };
        (dir, paths)
    }

    fn test_state() -> AgentState {
        AgentState {
            device_id: "test-device".to_string(),
            agent_version: "0.0.0-test".to_string(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn test_queue(paths: &Paths) -> Queue {
        Queue::open_with_key(paths, &test_state(), [7u8; 32]).unwrap()
    }

    #[test]
    fn enqueue_fetch_delete_roundtrip() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let id = q.enqueue_json(br#"{"a":1}"#).unwrap();
        let batch = q.fetch_batch_decrypted(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].0, id);
        assert_eq!(batch[0].1, br#"{"a":1}"#);
        let deleted = q.delete_ids(&[id]).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(q.queue_len().unwrap(), 0);
    }

    #[test]
    fn increment_attempts_increments() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let id1 = q.enqueue_json(b"{}").unwrap();
        let id2 = q.enqueue_json(b"{}").unwrap();
        q.increment_attempts(&[id1, id2]).unwrap();
        q.increment_attempts(&[id1]).unwrap();
        let attempts1: i64 = q
            .conn
            .query_row("SELECT attempts FROM events WHERE id = ?1", params![id1], |r| r.get(0))
            .unwrap();
        assert_eq!(attempts1, 2);
        let attempts2: i64 = q
            .conn
            .query_row("SELECT attempts FROM events WHERE id = ?1", params![id2], |r| r.get(0))
            .unwrap();
        assert_eq!(attempts2, 1);
    }

    #[test]
    fn gc_prunes_by_age() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let now = now_ms();
        let old_id = q.enqueue_json_at(b"{}", now.saturating_sub(100_000)).unwrap();
        let new_id = q.enqueue_json_at(b"{}", now).unwrap();
        let stats = q
            .gc(&GcConfig { max_age_ms: 50_000, max_rows: 1_000_000, max_attempts: 1000 })
            .unwrap();
        assert_eq!(stats.pruned_age, 1);
        assert_eq!(stats.pruned_attempts, 0);
        assert_eq!(stats.pruned_overflow, 0);
        let remaining_ids: Vec<i64> = q
            .fetch_batch_decrypted(10)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(remaining_ids, vec![new_id]);
        assert!(!remaining_ids.contains(&old_id));
    }

    #[test]
    fn gc_prunes_by_attempts() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let id1 = q.enqueue_json(b"{}").unwrap();
        let id2 = q.enqueue_json(b"{}").unwrap();
        for _ in 0..5 {
            q.increment_attempts(&[id1]).unwrap();
        }
        let stats = q
            .gc(&GcConfig { max_age_ms: u64::MAX, max_rows: 1_000_000, max_attempts: 3 })
            .unwrap();
        assert_eq!(stats.pruned_attempts, 1);
        let remaining_ids: Vec<i64> = q
            .fetch_batch_decrypted(10)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(remaining_ids, vec![id2]);
    }

    #[test]
    fn gc_keeps_newest_max_rows_on_overflow() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let now = now_ms();
        let mut ids = Vec::new();
        for i in 0..5u64 {
            ids.push(q.enqueue_json_at(b"{}", now + i).unwrap());
        }
        let stats = q
            .gc(&GcConfig { max_age_ms: u64::MAX, max_rows: 2, max_attempts: 1000 })
            .unwrap();
        assert_eq!(stats.pruned_overflow, 3);
        let remaining_ids: Vec<i64> = q
            .fetch_batch_decrypted(10)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(remaining_ids, vec![ids[3], ids[4]]);
    }

    #[test]
    fn fetch_batch_decrypted_skips_and_deletes_poison_row() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let good_id = q.enqueue_json(br#"{"ok":true}"#).unwrap();
        q.conn
            .execute(
                "INSERT INTO events(created_at, attempts, payload) VALUES (?1, 0, ?2)",
                params![now_ms() as i64, b"not encrypted garbage".to_vec()],
            )
            .unwrap();
        assert_eq!(q.queue_len().unwrap(), 2);
        let batch = q.fetch_batch_decrypted(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].0, good_id);
        assert_eq!(q.queue_len().unwrap(), 1, "poison row must be deleted");
    }
}
