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
            CREATE TABLE IF NOT EXISTS goals (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                target_lb   REAL NOT NULL,
                target_date TEXT,
                start_lb    REAL NOT NULL,
                start_date  TEXT NOT NULL,
                created_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;

        // One-time migration from the old single-row `goal` table (t-fit
        // before multiple goals). Whatever was set becomes the first row
        // of history — it's already the only row, so it's automatically
        // "current". The old table is dropped once this runs, so every
        // later start finds none and skips straight past.
        let had_old_table: i64 = c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='goal'",
            [],
            |r| r.get(0),
        )?;
        if had_old_table > 0 {
            c.execute(
                "INSERT INTO goals (target_lb, target_date, start_lb, start_date)
                 SELECT target_lb, target_date, start_lb, start_date FROM goal
                 WHERE target_lb IS NOT NULL AND start_lb IS NOT NULL AND start_date IS NOT NULL",
                [],
            )?;
            c.execute("DROP TABLE goal", [])?;
        }
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

    /// Every goal, newest first — so `.first()` is always the one currently
    /// being pursued, with no separate "which one is current" bookkeeping
    /// to keep in sync.
    pub fn goals(&self) -> Result<Vec<Goal>> {
        let c = self.0.lock().unwrap();
        let mut st = c.prepare(
            "SELECT id, target_lb, target_date, start_lb, start_date
             FROM goals ORDER BY id DESC",
        )?;
        let rows = st
            .query_map([], |r| {
                let pd = |s: Option<String>| {
                    s.and_then(|v| NaiveDate::parse_from_str(&v, "%Y-%m-%d").ok())
                };
                let start_date: String = r.get(4)?;
                Ok(Goal {
                    id: r.get(0)?,
                    target_lb: r.get(1)?,
                    target_date: pd(r.get(2)?),
                    start_lb: r.get(3)?,
                    start_date: NaiveDate::parse_from_str(&start_date, "%Y-%m-%d")
                        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn current_goal(&self) -> Result<Option<Goal>> {
        Ok(self.goals()?.into_iter().next())
    }

    pub fn add_goal(
        &self,
        target_lb: f64,
        target_date: Option<NaiveDate>,
        start_lb: f64,
        start_date: NaiveDate,
    ) -> Result<Goal> {
        let c = self.0.lock().unwrap();
        c.execute(
            "INSERT INTO goals (target_lb, target_date, start_lb, start_date, created_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))",
            params![
                target_lb,
                target_date.map(|d| d.format("%Y-%m-%d").to_string()),
                start_lb,
                start_date.format("%Y-%m-%d").to_string(),
            ],
        )?;
        Ok(Goal {
            id: c.last_insert_rowid(),
            target_lb,
            target_date,
            start_lb,
            start_date,
        })
    }

    /// Edits a goal in place — no new history entry. For fixing a typo,
    /// not for retiring one goal and starting the next (that's `add_goal`).
    pub fn update_goal(
        &self,
        id: i64,
        target_lb: f64,
        target_date: Option<NaiveDate>,
        start_lb: f64,
        start_date: NaiveDate,
    ) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "UPDATE goals SET target_lb=?1, target_date=?2, start_lb=?3, start_date=?4 WHERE id=?5",
            params![
                target_lb,
                target_date.map(|d| d.format("%Y-%m-%d").to_string()),
                start_lb,
                start_date.format("%Y-%m-%d").to_string(),
                id,
            ],
        )?;
        Ok(n > 0)
    }

    pub fn delete_goal(&self, id: i64) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute("DELETE FROM goals WHERE id = ?1", params![id])?;
        Ok(n > 0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_database_has_no_goals() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.current_goal().unwrap().is_none());
        assert!(db.goals().unwrap().is_empty());
    }

    /// The real thing this guards: an existing t-fit.sqlite3 — with a
    /// real goal already set — must come through an upgrade with that
    /// goal intact as history, not silently dropped.
    #[test]
    fn migrates_the_old_single_row_goal_table_into_history() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE goal (
                id          INTEGER PRIMARY KEY CHECK (id = 1),
                target_lb   REAL,
                target_date TEXT,
                start_lb    REAL,
                start_date  TEXT
            );
            INSERT INTO goal (id, target_lb, target_date, start_lb, start_date)
            VALUES (1, 190.0, '2026-01-01', 210.0, '2025-06-01');",
        )
        .unwrap();
        let db = Db(Arc::new(Mutex::new(conn)));

        db.migrate().unwrap();

        let goals = db.goals().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].target_lb, 190.0);
        assert_eq!(goals[0].start_lb, 210.0);
        assert_eq!(goals[0].start_date, NaiveDate::from_ymd_opt(2025, 6, 1).unwrap());

        let old_table_gone: i64 = {
            let c = db.0.lock().unwrap();
            c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='goal'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(old_table_gone, 0);

        // Running it again (every later startup) must not duplicate the row.
        db.migrate().unwrap();
        assert_eq!(db.goals().unwrap().len(), 1);
    }

    #[test]
    fn a_new_goal_becomes_current_and_the_old_one_becomes_history() {
        let db = Db::open_in_memory().unwrap();
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();

        let first = db.add_goal(190.0, None, 210.0, d("2025-06-01")).unwrap();
        let second = db.add_goal(180.0, None, 190.0, d("2026-01-01")).unwrap();

        let current = db.current_goal().unwrap().unwrap();
        assert_eq!(current.id, second.id);

        let all = db.goals().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, second.id); // newest first
        assert_eq!(all[1].id, first.id);
    }

    #[test]
    fn deleting_a_goal_removes_only_that_one() {
        let db = Db::open_in_memory().unwrap();
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let a = db.add_goal(190.0, None, 210.0, d("2025-06-01")).unwrap();
        let b = db.add_goal(180.0, None, 190.0, d("2026-01-01")).unwrap();

        assert!(db.delete_goal(a.id).unwrap());
        let remaining = db.goals().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, b.id);
    }
}
