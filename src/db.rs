//! SQLite storage. One file, no server, trivially backed up by copying it.

use crate::model::{Entry, Goal, Source};
use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Db(Arc::new(Mutex::new(conn)));
        db.migrate()?;
        Ok(db)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open_in_memory() -> Result<Self> {
        let db = Db(Arc::new(Mutex::new(Connection::open_in_memory()?)));
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let c = self.0.lock().unwrap();
        c.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS weight (
                day        TEXT PRIMARY KEY,
                weight_lb  REAL NOT NULL,
                memo       TEXT NOT NULL DEFAULT '',
                source     TEXT NOT NULL DEFAULT 'manual',
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS goal (
                id          INTEGER PRIMARY KEY CHECK (id = 1),
                target_lb   REAL,
                target_date TEXT,
                start_lb    REAL,
                start_date  TEXT
            );
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT OR IGNORE INTO goal (id) VALUES (1);
            "#,
        )?;
        Ok(())
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        let c = self.0.lock().unwrap();
        let mut st =
            c.prepare("SELECT day, weight_lb, memo, source FROM weight ORDER BY day ASC")?;
        let rows = st
            .query_map([], |r| {
                let day: String = r.get(0)?;
                Ok(Entry {
                    date: NaiveDate::parse_from_str(&day, "%Y-%m-%d")
                        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                    weight_lb: r.get(1)?,
                    memo: r.get(2)?,
                    source: Source::parse(&r.get::<_, String>(3)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Insert or update one day. Returns true if this changed anything.
    pub fn upsert(&self, e: &Entry) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "INSERT INTO weight (day, weight_lb, memo, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(day) DO UPDATE SET
                weight_lb = excluded.weight_lb,
                memo      = CASE WHEN excluded.memo <> '' THEN excluded.memo ELSE weight.memo END,
                source    = excluded.source,
                updated_at= datetime('now')
             WHERE weight.weight_lb <> excluded.weight_lb
                OR (excluded.memo <> '' AND weight.memo <> excluded.memo)",
            params![
                e.date.format("%Y-%m-%d").to_string(),
                e.weight_lb,
                e.memo,
                e.source.as_str()
            ],
        )?;
        Ok(n > 0)
    }

    /// Insert only if the day is absent. Used by sync so it can never
    /// clobber something you typed yourself.
    pub fn insert_if_absent(&self, e: &Entry) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO weight (day, weight_lb, memo, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))",
            params![
                e.date.format("%Y-%m-%d").to_string(),
                e.weight_lb,
                e.memo,
                e.source.as_str()
            ],
        )?;
        Ok(n > 0)
    }

    pub fn delete(&self, day: NaiveDate) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "DELETE FROM weight WHERE day = ?1",
            params![day.format("%Y-%m-%d").to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn goal(&self) -> Result<Goal> {
        let c = self.0.lock().unwrap();
        let g = c
            .query_row(
                "SELECT target_lb, target_date, start_lb, start_date FROM goal WHERE id = 1",
                [],
                |r| {
                    let pd = |s: Option<String>| {
                        s.and_then(|v| NaiveDate::parse_from_str(&v, "%Y-%m-%d").ok())
                    };
                    Ok(Goal {
                        target_lb: r.get(0)?,
                        target_date: pd(r.get(1)?),
                        start_lb: r.get(2)?,
                        start_date: pd(r.get(3)?),
                    })
                },
            )
            .optional()?;
        Ok(g.unwrap_or_default())
    }

    pub fn set_goal(&self, g: &Goal) -> Result<()> {
        let c = self.0.lock().unwrap();
        c.execute(
            "UPDATE goal SET target_lb=?1, target_date=?2, start_lb=?3, start_date=?4 WHERE id=1",
            params![
                g.target_lb,
                g.target_date.map(|d| d.format("%Y-%m-%d").to_string()),
                g.start_lb,
                g.start_date.map(|d| d.format("%Y-%m-%d").to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let c = self.0.lock().unwrap();
        Ok(c.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let c = self.0.lock().unwrap();
        c.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn del_setting(&self, key: &str) -> Result<()> {
        let c = self.0.lock().unwrap();
        c.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }
}
