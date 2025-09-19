use anyhow::Result;
use rusqlite::{params, Connection};

#[derive(Debug)]
pub struct FocusStore {
    conn: Connection,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FocusBlockRow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub dur_ms: i64,
    pub app_name: String,
    pub window_title: String,
}

impl FocusStore {
    pub fn open(paths: &crate::paths::Paths) -> Result<Self> {
        // Reutilizamos queue.sqlite para simplificar despliegue
        let conn = Connection::open(paths.queue_db())?;
        conn.pragma_update(None, "journal_mode", &"WAL")?;
        conn.pragma_update(None, "synchronous", &"NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS focus_blocks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                start_ms INTEGER NOT NULL,
                end_ms INTEGER NOT NULL,
                dur_ms INTEGER NOT NULL,
                app_name TEXT NOT NULL,
                window_title TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_focus_end ON focus_blocks(end_ms DESC);
            ",
        )?;
        Ok(Self { conn })
    }

    pub fn insert_block(&self, b: &FocusBlockRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO focus_blocks(start_ms,end_ms,dur_ms,app_name,window_title) VALUES (?1,?2,?3,?4,?5)",
            params![b.start_ms, b.end_ms, b.dur_ms, b.app_name, b.window_title],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_recent(&self, limit: usize, offset: usize) -> Result<Vec<FocusBlockRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT start_ms,end_ms,dur_ms,app_name,window_title FROM focus_blocks ORDER BY end_ms DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
            Ok(FocusBlockRow {
                start_ms: row.get(0)?,
                end_ms: row.get(1)?,
                dur_ms: row.get(2)?,
                app_name: row.get(3)?,
                window_title: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows { out.push(r?); }
        Ok(out)
    }

    pub fn prune_older_than(&self, keep_latest: usize) -> Result<usize> {
        // elimina todo menos los N más recientes por end_ms
        let mut deleted = 0usize;
        let mut stmt = self.conn.prepare("SELECT id FROM focus_blocks ORDER BY end_ms DESC LIMIT -1 OFFSET ?1")?;
        let ids = stmt.query_map([keep_latest as i64], |row| row.get::<_, i64>(0))?;
        let tx = self.conn.unchecked_transaction()?;
        let mut dstmt = self.conn.prepare("DELETE FROM focus_blocks WHERE id = ?1")?;
        for id in ids { let id = id?; dstmt.execute([id])?; deleted += 1; }
        tx.commit()?;
        Ok(deleted)
    }
}


#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FocusAggregateRow {
    pub day: String,
    pub app_name: String,
    pub dur_ms: i64,
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl FocusStore {
    pub fn aggregate_last_days_by_app(&self, days: u32) -> anyhow::Result<Vec<FocusAggregateRow>> {
        let cutoff = now_ms() - (days as i64) * 86_400_000;
        let mut stmt = self.conn.prepare(
            "SELECT date(end_ms/1000,'unixepoch') AS day, app_name, SUM(dur_ms) AS total_dur
             FROM focus_blocks
             WHERE end_ms >= ?1
             GROUP BY day, app_name
             ORDER BY day DESC, total_dur DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![cutoff], |row| {
            Ok(FocusAggregateRow { day: row.get(0)?, app_name: row.get(1)?, dur_ms: row.get(2)? })
        })?;
        let mut out = Vec::new();
        for r in rows { out.push(r?); }
        Ok(out)
    }
}
