use crate::crypto::{decrypt_decompress, encrypt_compress};
use crate::paths::Paths;
use crate::state::AgentState;
use anyhow::Result;
use rusqlite::{params, Connection};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Timestamp (ms) del último `error!` de fallo sistémico de descifrado
/// emitido, para limitar la frecuencia de log entre construcciones
/// sucesivas de `Queue` (una por cada `open_with_key`).
static LAST_SYSTEMIC_LOG_MS: AtomicU64 = AtomicU64::new(0);

/// Intervalo mínimo entre líneas de log del fallo sistémico de descifrado.
const SYSTEMIC_LOG_THROTTLE_MS: u64 = 60_000;

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

/// Añade una columna a `events` si falta. Idempotente y a prueba de carreras:
/// varias conexiones (WAL) pueden abrir la misma base a la vez; si otra ya
/// añadió la columna entre el chequeo y el `ALTER`, se tolera el error benigno
/// `duplicate column name` en vez de fallar la apertura (que descartaría un
/// evento en el loop de captura).
fn ensure_column(conn: &Connection, name: &str, decl: &str) -> Result<()> {
    let exists: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('events') WHERE name = ?1")?
        .exists([name])?;
    if exists {
        return Ok(());
    }
    match conn.execute(&format!("ALTER TABLE events ADD COLUMN {decl}"), []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(m))) if m.contains("duplicate column name") => {
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
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
            quarantined INTEGER NOT NULL DEFAULT 0,
            quarantined_by TEXT,
            payload BLOB NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_created ON events(created_at);
        CREATE INDEX IF NOT EXISTS idx_events_attempts ON events(attempts);
        ",
    )?;
    // Migración de bases creadas antes de estas columnas.
    ensure_column(conn, "quarantined", "quarantined INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "quarantined_by", "quarantined_by TEXT")?;
    // Índice del barrido caliente: filas entregables (quarantined=0) por edad.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_events_live ON events(quarantined, created_at);",
    )?;
    Ok(())
}

impl Queue {
    /// Abre la cola con una clave ya cargada por el llamador (evita
    /// tocar el keystore del SO en cada apertura). Usado por el daemon, que
    /// carga la clave una sola vez al arrancar; también usado en tests para
    /// evitar el Keychain real.
    pub fn open_with_key(paths: &Paths, state: &AgentState, key: [u8; 32]) -> Result<Self> {
        let conn = Connection::open(paths.queue_db())?;
        init_schema(&conn)?;
        // Recuperación en cambio de identidad: si la identidad activa difiere
        // de la que puso una fila en cuarentena, esa fila se reconsidera bajo
        // la identidad actual — pudo volverse recuperable (p.ej. se restauró un
        // `state.json` anterior). La cuarentena tiene alcance de identidad, no
        // es permanente. Preserva el invariante de no perder datos recuperables.
        conn.execute(
            "UPDATE events SET quarantined = 0, quarantined_by = NULL \
             WHERE quarantined = 1 AND quarantined_by IS NOT ?1",
            params![state.device_id],
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

    /// Payload crudo (cifrado), sin filtrar cuarentena. NO usar para entrega:
    /// el sender debe usar `fetch_batch_decrypted`, que salta filas poison. Se
    /// mantiene para inspección/depuración de blobs.
    pub fn fetch_batch(&self, limit: usize) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, payload FROM events ORDER BY created_at ASC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for r in rows { out.push(r?); }
        Ok(out)
    }

    /// Igual que `fetch_batch`, pero descifrado y saltando filas en cuarentena.
    ///
    /// Las filas que no descifran nunca se borran (borrar ante un error de
    /// descifrado arriesga pérdida de datos si el fallo es transitorio, p.ej.
    /// clave equivocada cargada un momento). En su lugar:
    ///
    /// - Si la tanda tiene alguna fila que sí descifra (o la fila más nueva de
    ///   la cola descifra), la identidad (clave+AAD) está sana: las que fallan
    ///   son poison permanente (p.ej. `device_id` cambió → AAD viejo) → se
    ///   ponen en cuarentena para que dejen de bloquear a las buenas de atrás.
    /// - Si TODA la vista actual es indescifrable y la fila más nueva también
    ///   falla, puede ser una clave global equivocada → no se toca nada y se
    ///   devuelve vacío: un reinicio con la clave correcta recupera todo.
    ///
    /// La GC por edad (`gc`) es la única vía que borra las filas en cuarentena.
    pub fn fetch_batch_decrypted(&self, limit: usize) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut out = Vec::new();
        let mut failed_ids: Vec<i64> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT id, payload FROM events WHERE quarantined = 0 ORDER BY created_at ASC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            for r in rows {
                let (id, blob) = r?;
                match crate::crypto::decrypt_decompress(&self.key, &self.aad, &blob) {
                    Ok(plain) => out.push((id, plain)),
                    Err(_) => failed_ids.push(id),
                }
            }
        }
        if failed_ids.is_empty() {
            return Ok(out);
        }
        // Alguna fila descifró en esta tanda, o la más nueva sí lo hace: la
        // identidad está sana, así que los fallos son poison genuino.
        let key_healthy = !out.is_empty() || self.newest_live_row_decrypts()?;
        if !key_healthy {
            self.note_systemic_decrypt_failure(failed_ids.len());
            return Ok(Vec::new());
        }
        self.quarantine_ids(&failed_ids)?;
        tracing::warn!(
            count = failed_ids.len(),
            "filas indescifrables puestas en cuarentena (no se borran; ¿device_id cambió?)"
        );
        Ok(out)
    }

    /// ¿La fila más reciente no-cuarentenada descifra con la identidad actual?
    /// Prueba barata para distinguir poison localizado (viejo) de una clave
    /// global equivocada. `false` si no hay filas vivas.
    fn newest_live_row_decrypts(&self) -> Result<bool> {
        let mut stmt = self.conn.prepare(
            "SELECT payload FROM events WHERE quarantined = 0 ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        match rows.next() {
            Some(r) => {
                let blob = r?;
                Ok(crate::crypto::decrypt_decompress(&self.key, &self.aad, &blob).is_ok())
            }
            None => Ok(false),
        }
    }

    /// Marca filas como en cuarentena bajo la identidad actual (`quarantined_by`
    /// = device_id). No se vuelven a leer ni se borran salvo por GC de edad;
    /// si la identidad cambia, `open_with_key` las reconsidera.
    fn quarantine_ids(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql =
            format!("UPDATE events SET quarantined = 1, quarantined_by = ? WHERE id IN ({placeholders})");
        let dev = String::from_utf8_lossy(&self.aad).into_owned();
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
        params.push(&dev);
        for id in ids {
            params.push(id);
        }
        let n = self.conn.execute(&sql, params.as_slice())?;
        Ok(n)
    }

    /// Log throttleado (1/min) del fallo sistémico de descifrado.
    fn note_systemic_decrypt_failure(&self, total: usize) {
        let now = now_ms();
        let last = LAST_SYSTEMIC_LOG_MS.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= SYSTEMIC_LOG_THROTTLE_MS {
            LAST_SYSTEMIC_LOG_MS.store(now, Ordering::Relaxed);
            tracing::error!(
                total,
                "fallo sistémico de descifrado en la cola (¿clave global incorrecta?); no se toca nada, esperando reinicio"
            );
        }
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
            .prepare("SELECT payload FROM events WHERE quarantined = 0 ORDER BY created_at ASC LIMIT ?1")?;
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
            .prepare("SELECT payload FROM events WHERE quarantined = 0 ORDER BY created_at DESC LIMIT ?1")?;
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

    fn state_dev(device: &str) -> AgentState {
        AgentState {
            device_id: device.to_string(),
            agent_version: "0.0.0-test".to_string(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn queue_dev(paths: &Paths, key: [u8; 32], device: &str) -> Queue {
        Queue::open_with_key(paths, &state_dev(device), key).unwrap()
    }

    fn insert_garbage(q: &Queue) {
        q.conn
            .execute(
                "INSERT INTO events(created_at, attempts, payload) VALUES (?1, 0, ?2)",
                params![now_ms() as i64, b"not encrypted garbage".to_vec()],
            )
            .unwrap();
    }

    fn count_quarantined(q: &Queue) -> i64 {
        q.conn
            .query_row("SELECT COUNT(1) FROM events WHERE quarantined = 1", [], |r| r.get(0))
            .unwrap()
    }

    /// Con la clave sana (una fila de la tanda sí descifra), las filas
    /// indescifrables se ponen en cuarentena — nunca se borran — y las buenas
    /// se entregan igual.
    #[test]
    fn poison_row_quarantined_not_deleted_good_delivered() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let good_id = q.enqueue_json(br#"{"ok":true}"#).unwrap();
        insert_garbage(&q);
        assert_eq!(q.queue_len().unwrap(), 2);

        let batch = q.fetch_batch_decrypted(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].0, good_id);
        assert_eq!(q.queue_len().unwrap(), 2, "poison row must NOT be deleted");
        assert_eq!(count_quarantined(&q), 1, "poison row must be quarantined");
    }

    /// Prefijo envenenado más largo que la ventana de la tanda: mientras la
    /// fila más nueva sí descifre (identidad sana), la cuarentena avanza tanda
    /// a tanda hasta alcanzar las filas buenas de atrás. Nada se borra.
    #[test]
    fn poison_prefix_quarantined_until_good_rows_reached() {
        let (_d, paths) = tmp_paths();
        // 6 filas viejas cifradas con device "dev-old".
        let q_old = queue_dev(&paths, [7u8; 32], "dev-old");
        let base = now_ms();
        for i in 0..6u64 {
            q_old
                .enqueue_json_at(format!(r#"{{"old":{i}}}"#).as_bytes(), base + i)
                .unwrap();
        }
        // Cambia el device (AAD): las 6 viejas ahora son indescifrables.
        // 3 filas buenas nuevas bajo el device nuevo.
        let q_new = queue_dev(&paths, [7u8; 32], "dev-new");
        for i in 0..3u64 {
            q_new
                .enqueue_json_at(format!(r#"{{"new":{i}}}"#).as_bytes(), base + 100 + i)
                .unwrap();
        }
        assert_eq!(q_new.queue_len().unwrap(), 9);

        // Tanda de 3: primeras tandas son 100% poison → se ponen en cuarentena
        // y devuelven vacío; eventualmente aparecen las 3 buenas.
        let mut delivered: Vec<(i64, Vec<u8>)> = Vec::new();
        for _ in 0..5 {
            let batch = q_new.fetch_batch_decrypted(3).unwrap();
            if !batch.is_empty() {
                delivered = batch;
                break;
            }
        }
        assert_eq!(delivered.len(), 3, "las 3 buenas deben entregarse");
        assert!(delivered.iter().all(|(_, p)| p.starts_with(b"{\"new\":")));
        assert_eq!(count_quarantined(&q_new), 6, "las 6 viejas en cuarentena");
        assert_eq!(q_new.queue_len().unwrap(), 9, "nada se borra");
    }

    /// Fallo sistémico global: TODA la cola es indescifrable y la fila más
    /// nueva también falla → puede ser una clave equivocada transitoria. No se
    /// pone nada en cuarentena ni se borra: se bloquea la entrega y se espera
    /// un reinicio con la clave correcta, que recupera todo.
    #[test]
    fn systemic_wrong_key_blocks_and_preserves_everything() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths); // key = [7u8; 32]
        for i in 0..5 {
            q.enqueue_json(format!(r#"{{"i":{i}}}"#).as_bytes()).unwrap();
        }
        assert_eq!(q.queue_len().unwrap(), 5);

        // Reabrir con clave distinta: cada fila falla la autenticación AEAD.
        let q2 = Queue::open_with_key(&paths, &test_state(), [9u8; 32]).unwrap();
        let batch = q2.fetch_batch_decrypted(10).unwrap();
        assert!(batch.is_empty(), "systemic failure must not return rows");
        assert_eq!(q2.queue_len().unwrap(), 5, "must not delete any row");
        assert_eq!(count_quarantined(&q2), 0, "must not quarantine on wrong key");

        // Reinicio con la clave correcta: todo se recupera.
        let q3 = Queue::open_with_key(&paths, &test_state(), [7u8; 32]).unwrap();
        let recovered = q3.fetch_batch_decrypted(10).unwrap();
        assert_eq!(recovered.len(), 5, "correct key recovers all rows");
    }

    /// Poison parcial: algunas filas indescifrables junto a buenas → las buenas
    /// se entregan, las malas se ponen en cuarentena (no se borran).
    #[test]
    fn partial_poison_quarantines_bad_keeps_good() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let mut good_ids: Vec<i64> = Vec::new();
        for i in 0..3 {
            good_ids.push(q.enqueue_json(format!(r#"{{"i":{i}}}"#).as_bytes()).unwrap());
        }
        insert_garbage(&q);
        insert_garbage(&q);
        assert_eq!(q.queue_len().unwrap(), 5);

        let batch = q.fetch_batch_decrypted(10).unwrap();
        let mut ids: Vec<i64> = batch.into_iter().map(|(id, _)| id).collect();
        ids.sort();
        let mut expected = good_ids.clone();
        expected.sort();
        assert_eq!(ids, expected, "partial poison must keep only good rows");
        assert_eq!(q.queue_len().unwrap(), 5, "garbage rows must NOT be deleted");
        assert_eq!(count_quarantined(&q), 2, "garbage rows must be quarantined");
    }

    /// Cola totalmente mala sin ninguna fila buena (ni la más nueva descifra):
    /// se trata como fallo sistémico → se bloquea, no se borra ni pone en
    /// cuarentena. La GC por edad la limpiará a los 14 días.
    #[test]
    fn all_bad_no_good_row_blocks_without_delete() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        for _ in 0..3 {
            insert_garbage(&q);
        }
        assert_eq!(q.queue_len().unwrap(), 3);

        let batch = q.fetch_batch_decrypted(10).unwrap();
        assert!(batch.is_empty());
        assert_eq!(q.queue_len().unwrap(), 3, "all-bad batch must NOT be deleted");
        assert_eq!(count_quarantined(&q), 0, "nothing quarantined when key looks unhealthy");
    }

    /// Las filas en cuarentena no vuelven a aparecer en fetch posteriores.
    #[test]
    fn quarantined_rows_excluded_from_future_fetch() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        let good_id = q.enqueue_json(br#"{"ok":1}"#).unwrap();
        insert_garbage(&q);

        let first = q.fetch_batch_decrypted(10).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(count_quarantined(&q), 1);

        // Entrega la buena; solo queda la fila en cuarentena.
        q.delete_ids(&[good_id]).unwrap();
        let second = q.fetch_batch_decrypted(10).unwrap();
        assert!(second.is_empty(), "quarantined row must not be re-fetched");
    }

    /// La GC por edad borra filas en cuarentena igual que cualquier otra
    /// (opera sobre created_at, sin descifrar).
    #[test]
    fn gc_prunes_quarantined_by_age() {
        let (_d, paths) = tmp_paths();
        let q = test_queue(&paths);
        insert_garbage(&q); // created_at = now (reciente)
        // Envejece la fila y ponla en cuarentena.
        q.conn
            .execute(
                "UPDATE events SET created_at = ?1, quarantined = 1",
                params![now_ms().saturating_sub(100_000) as i64],
            )
            .unwrap();
        let stats = q
            .gc(&GcConfig { max_age_ms: 50_000, max_rows: 1_000_000, max_attempts: 1000 })
            .unwrap();
        assert_eq!(stats.pruned_age, 1, "quarantined row aged out by GC");
        assert_eq!(q.queue_len().unwrap(), 0);
    }

    /// Una base creada antes de la columna `quarantined` recibe la columna al
    /// abrir (migración) y la cuarentena funciona sobre ella.
    #[test]
    fn legacy_db_without_quarantined_column_migrates() {
        let (_d, paths) = tmp_paths();
        // Esquema viejo: sin columna `quarantined`.
        {
            let conn = Connection::open(paths.queue_db()).unwrap();
            conn.execute_batch(
                "CREATE TABLE events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at INTEGER NOT NULL,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    payload BLOB NOT NULL
                );",
            )
            .unwrap();
        }
        // Abrir vía Queue → migración añade la columna.
        let q = test_queue(&paths);
        let good_id = q.enqueue_json(br#"{"ok":1}"#).unwrap();
        insert_garbage(&q);
        let batch = q.fetch_batch_decrypted(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].0, good_id);
        assert_eq!(count_quarantined(&q), 1, "migración + cuarentena funcionan");
    }

    /// La cuarentena tiene alcance de identidad: si se restaura un `state.json`
    /// anterior (device_id vuelve al viejo), las filas que se habían puesto en
    /// cuarentena bajo la identidad nueva se reconsideran y se entregan — no se
    /// pierden. (Cierra el hallazgo #1 del review adversarial.)
    #[test]
    fn quarantine_recovered_after_identity_restore() {
        let (_d, paths) = tmp_paths();
        let base = now_ms();
        // 6 filas bajo device "dev-A".
        let mut a_ids: Vec<i64> = Vec::new();
        {
            let q_a = queue_dev(&paths, [7u8; 32], "dev-A");
            for i in 0..6u64 {
                a_ids.push(
                    q_a.enqueue_json_at(format!(r#"{{"a":{i}}}"#).as_bytes(), base + i)
                        .unwrap(),
                );
            }
        }
        // Cambia a device "dev-B", encola 3 buenas y drena: las 6 de A quedan
        // en cuarentena bajo dev-B.
        {
            let q_b = queue_dev(&paths, [7u8; 32], "dev-B");
            for i in 0..3u64 {
                q_b.enqueue_json_at(format!(r#"{{"b":{i}}}"#).as_bytes(), base + 100 + i)
                    .unwrap();
            }
            let mut delivered = 0;
            for _ in 0..5 {
                delivered = q_b.fetch_batch_decrypted(100).unwrap().len();
                if delivered > 0 {
                    break;
                }
            }
            assert_eq!(delivered, 3, "las 3 de B se entregan");
            assert_eq!(count_quarantined(&q_b), 6, "las 6 de A en cuarentena bajo B");
        }
        // Restaura device "dev-A": al abrir, las 6 en cuarentena (quarantined_by
        // = dev-B) se reconsideran; ahora descifran y se entregan.
        {
            let q_a2 = queue_dev(&paths, [7u8; 32], "dev-A");
            let delivered: Vec<i64> = q_a2
                .fetch_batch_decrypted(100)
                .unwrap()
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let mut got = delivered.clone();
            got.sort();
            let mut want = a_ids.clone();
            want.sort();
            assert_eq!(got, want, "las 6 de A se recuperan tras el restore");
        }
    }
}
