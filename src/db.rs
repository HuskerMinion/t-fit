//! SQLite storage. One file, no server, trivially backed up by copying it.

use crate::model::{Entry, Goal, Source, User};
use anyhow::{anyhow, bail, Result};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Per-user settings live in the same flat `settings` table as app-wide
/// ones, under a namespaced key — one less table, and `setting`/
/// `set_setting`/`del_setting` keep working unchanged underneath.
fn user_key(user_id: i64, key: &str) -> String {
    format!("u{user_id}.{key}")
}

/// True if `table` already has a column named `column`. Used during
/// migration to tell an old-shape table from one already upgraded.
fn table_has_column(c: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut st = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = st
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cols.iter().any(|c| c == column))
}

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
        c.pragma_update(None, "foreign_keys", "OFF")?;

        c.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                name       TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;

        // Every database needs at least one profile to attribute data to —
        // a brand-new install as much as one upgrading from single-user
        // t-fit, where this becomes the home for everything already there.
        let user_count: i64 = c.query_row("SELECT count(*) FROM users", [], |r| r.get(0))?;
        let default_id: i64 = if user_count == 0 {
            c.execute(
                "INSERT INTO users (name, created_at) VALUES ('Me', datetime('now'))",
                [],
            )?;
            c.last_insert_rowid()
        } else {
            c.query_row("SELECT id FROM users ORDER BY id LIMIT 1", [], |r| r.get(0))?
        };

        // `weight` needs a composite (user_id, day) key — two people can
        // both log the same day — so an old-shape table (day alone as the
        // primary key) has to be rebuilt rather than just ALTERed.
        let weight_exists: i64 = c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='weight'",
            [],
            |r| r.get(0),
        )?;
        let weight_shape = r#"
            user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            day        TEXT NOT NULL,
            weight_lb  REAL NOT NULL,
            memo       TEXT NOT NULL DEFAULT '',
            source     TEXT NOT NULL DEFAULT 'manual',
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (user_id, day)
        "#;
        if weight_exists == 0 {
            c.execute_batch(&format!("CREATE TABLE weight ({weight_shape});"))?;
        } else if !table_has_column(&c, "weight", "user_id")? {
            c.execute_batch(&format!("CREATE TABLE weight_new ({weight_shape});"))?;
            c.execute(
                &format!(
                    "INSERT INTO weight_new (user_id, day, weight_lb, memo, source, updated_at)
                     SELECT {default_id}, day, weight_lb, memo, source, updated_at FROM weight"
                ),
                [],
            )?;
            c.execute_batch("DROP TABLE weight; ALTER TABLE weight_new RENAME TO weight;")?;
        }

        // `goals.id` is already its own standalone primary key, so a plain
        // ADD COLUMN + backfill is enough here — no rebuild needed.
        let goals_exists: i64 = c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='goals'",
            [],
            |r| r.get(0),
        )?;
        if goals_exists == 0 {
            c.execute_batch(
                r#"
                CREATE TABLE goals (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                    target_lb   REAL NOT NULL,
                    target_date TEXT,
                    start_lb    REAL NOT NULL,
                    start_date  TEXT NOT NULL,
                    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
                );
                "#,
            )?;
        } else if !table_has_column(&c, "goals", "user_id")? {
            c.execute("ALTER TABLE goals ADD COLUMN user_id INTEGER REFERENCES users(id)", [])?;
            c.execute(
                "UPDATE goals SET user_id = ?1 WHERE user_id IS NULL",
                params![default_id],
            )?;
        }

        // One-time migration from the old single-row `goal` table (t-fit
        // before multiple goals). Whatever was set becomes the first row
        // of history, owned by the default profile — it's already the only
        // row, so it's automatically "current". The old table is dropped
        // once this runs, so every later start finds none and skips
        // straight past.
        let had_old_goal_table: i64 = c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='goal'",
            [],
            |r| r.get(0),
        )?;
        if had_old_goal_table > 0 {
            c.execute(
                &format!(
                    "INSERT INTO goals (user_id, target_lb, target_date, start_lb, start_date)
                     SELECT {default_id}, target_lb, target_date, start_lb, start_date FROM goal
                     WHERE target_lb IS NOT NULL AND start_lb IS NOT NULL AND start_date IS NOT NULL"
                ),
                [],
            )?;
            c.execute("DROP TABLE goal", [])?;
        }

        // Move any pre-multi-user Withings setup — the app registration as
        // well as the tokens — onto the default profile, so upgrading
        // doesn't silently read as "not set up" until someone re-enters it.
        // Withings issues credentials per person, so the client id and
        // secret belong to whoever was using t-fit before the upgrade, not
        // to the household. Runs once: the old app-wide keys are gone after
        // the first pass.
        for key in [
            "withings.client_id",
            "withings.client_secret",
            "withings.access_token",
            "withings.refresh_token",
            "withings.expires_at",
            "withings.oauth_state",
            "withings.last_sync",
            "withings.last_error",
        ] {
            let old: Option<String> = c
                .query_row("SELECT value FROM settings WHERE key=?1", params![key], |r| r.get(0))
                .optional()?;
            if let Some(v) = old {
                let new_key = user_key(default_id, key);
                c.execute(
                    "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
                    params![new_key, v],
                )?;
                c.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
            }
        }

        c.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /* ── users ───────────────────────────────────────────────────── */

    /// Every profile, in the order they were created.
    pub fn users(&self) -> Result<Vec<User>> {
        let c = self.0.lock().unwrap();
        let mut st = c.prepare("SELECT id, name FROM users ORDER BY id ASC")?;
        let rows = st
            .query_map([], |r| Ok(User { id: r.get(0)?, name: r.get(1)? }))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn create_user(&self, name: &str) -> Result<User> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a name is required");
        }
        let c = self.0.lock().unwrap();
        let clash: i64 = c.query_row(
            "SELECT count(*) FROM users WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |r| r.get(0),
        )?;
        if clash > 0 {
            bail!("a profile named \"{name}\" already exists");
        }
        c.execute(
            "INSERT INTO users (name, created_at) VALUES (?1, datetime('now'))",
            params![name],
        )?;
        Ok(User { id: c.last_insert_rowid(), name: name.to_string() })
    }

    pub fn rename_user(&self, id: i64, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a name is required");
        }
        let c = self.0.lock().unwrap();
        let clash: i64 = c.query_row(
            "SELECT count(*) FROM users WHERE name = ?1 COLLATE NOCASE AND id <> ?2",
            params![name, id],
            |r| r.get(0),
        )?;
        if clash > 0 {
            bail!("a profile named \"{name}\" already exists");
        }
        let n = c.execute("UPDATE users SET name = ?1 WHERE id = ?2", params![name, id])?;
        if n == 0 {
            bail!("no such profile");
        }
        Ok(())
    }

    /// Deletes a profile and — via `ON DELETE CASCADE` — everything logged
    /// under it. Refuses to delete the last remaining profile, so the app
    /// is never left with nobody to attribute data to.
    pub fn delete_user(&self, id: i64) -> Result<()> {
        let c = self.0.lock().unwrap();
        let total: i64 = c.query_row("SELECT count(*) FROM users", [], |r| r.get(0))?;
        if total <= 1 {
            bail!("can't delete the only profile");
        }
        let n = c.execute("DELETE FROM users WHERE id = ?1", params![id])?;
        if n == 0 {
            bail!("no such profile");
        }
        // Namespaced settings (Withings tokens, etc.) aren't tied by a
        // foreign key, so they need their own cleanup.
        c.execute(
            "DELETE FROM settings WHERE key LIKE ?1",
            params![format!("u{id}.%")],
        )?;
        // If the profile just deleted was the active one, fall back to
        // whichever remains — otherwise the app would be pointed at a user
        // that no longer exists.
        let active: Option<String> = c
            .query_row("SELECT value FROM settings WHERE key = 'active_user_id'", [], |r| r.get(0))
            .optional()?;
        if active.as_deref() == Some(id.to_string().as_str()) {
            let fallback: i64 = c.query_row("SELECT id FROM users ORDER BY id LIMIT 1", [], |r| r.get(0))?;
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('active_user_id', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![fallback.to_string()],
            )?;
        }
        Ok(())
    }

    /// Whichever profile the app is currently showing. Self-healing: an
    /// unset or stale value (e.g. that profile was since deleted) falls
    /// back to the first one and persists that as the new choice.
    pub fn active_user_id(&self) -> Result<i64> {
        let c = self.0.lock().unwrap();
        let stored: Option<i64> = c
            .query_row("SELECT value FROM settings WHERE key = 'active_user_id'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|s| s.parse().ok());
        if let Some(id) = stored {
            let exists: i64 =
                c.query_row("SELECT count(*) FROM users WHERE id = ?1", params![id], |r| r.get(0))?;
            if exists > 0 {
                return Ok(id);
            }
        }
        let fallback: i64 = c
            .query_row("SELECT id FROM users ORDER BY id LIMIT 1", [], |r| r.get(0))
            .optional()?
            .ok_or_else(|| anyhow!("no user profiles exist"))?;
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('active_user_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![fallback.to_string()],
        )?;
        Ok(fallback)
    }

    pub fn set_active_user(&self, id: i64) -> Result<()> {
        let c = self.0.lock().unwrap();
        let exists: i64 =
            c.query_row("SELECT count(*) FROM users WHERE id = ?1", params![id], |r| r.get(0))?;
        if exists == 0 {
            bail!("no such profile");
        }
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('active_user_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /* ── entries ─────────────────────────────────────────────────── */

    pub fn entries(&self, user_id: i64) -> Result<Vec<Entry>> {
        let c = self.0.lock().unwrap();
        let mut st = c.prepare(
            "SELECT day, weight_lb, memo, source FROM weight WHERE user_id = ?1 ORDER BY day ASC",
        )?;
        let rows = st
            .query_map(params![user_id], |r| {
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
    pub fn upsert(&self, user_id: i64, e: &Entry) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "INSERT INTO weight (user_id, day, weight_lb, memo, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
             ON CONFLICT(user_id, day) DO UPDATE SET
                weight_lb = excluded.weight_lb,
                memo      = CASE WHEN excluded.memo <> '' THEN excluded.memo ELSE weight.memo END,
                source    = excluded.source,
                updated_at= datetime('now')
             WHERE weight.weight_lb <> excluded.weight_lb
                OR (excluded.memo <> '' AND weight.memo <> excluded.memo)",
            params![
                user_id,
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
    pub fn insert_if_absent(&self, user_id: i64, e: &Entry) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO weight (user_id, day, weight_lb, memo, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![
                user_id,
                e.date.format("%Y-%m-%d").to_string(),
                e.weight_lb,
                e.memo,
                e.source.as_str()
            ],
        )?;
        Ok(n > 0)
    }

    pub fn delete(&self, user_id: i64, day: NaiveDate) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "DELETE FROM weight WHERE user_id = ?1 AND day = ?2",
            params![user_id, day.format("%Y-%m-%d").to_string()],
        )?;
        Ok(n > 0)
    }

    /* ── goals ───────────────────────────────────────────────────── */

    /// Every goal for this profile, newest first — so `.first()` is always
    /// the one currently being pursued, with no separate "which one is
    /// current" bookkeeping to keep in sync.
    pub fn goals(&self, user_id: i64) -> Result<Vec<Goal>> {
        let c = self.0.lock().unwrap();
        let mut st = c.prepare(
            "SELECT id, target_lb, target_date, start_lb, start_date
             FROM goals WHERE user_id = ?1 ORDER BY id DESC",
        )?;
        let rows = st
            .query_map(params![user_id], |r| {
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

    pub fn current_goal(&self, user_id: i64) -> Result<Option<Goal>> {
        Ok(self.goals(user_id)?.into_iter().next())
    }

    pub fn add_goal(
        &self,
        user_id: i64,
        target_lb: f64,
        target_date: Option<NaiveDate>,
        start_lb: f64,
        start_date: NaiveDate,
    ) -> Result<Goal> {
        let c = self.0.lock().unwrap();
        c.execute(
            "INSERT INTO goals (user_id, target_lb, target_date, start_lb, start_date, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![
                user_id,
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
        user_id: i64,
        id: i64,
        target_lb: f64,
        target_date: Option<NaiveDate>,
        start_lb: f64,
        start_date: NaiveDate,
    ) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "UPDATE goals SET target_lb=?1, target_date=?2, start_lb=?3, start_date=?4
             WHERE id=?5 AND user_id=?6",
            params![
                target_lb,
                target_date.map(|d| d.format("%Y-%m-%d").to_string()),
                start_lb,
                start_date.format("%Y-%m-%d").to_string(),
                id,
                user_id,
            ],
        )?;
        Ok(n > 0)
    }

    pub fn delete_goal(&self, user_id: i64, id: i64) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let n = c.execute(
            "DELETE FROM goals WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
        )?;
        Ok(n > 0)
    }

    /* ── settings ────────────────────────────────────────────────── */

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

    /// Same as `setting`/`set_setting`/`del_setting`, but namespaced to one
    /// profile — how the Withings link is kept separate per person without
    /// a dedicated table.
    pub fn user_setting(&self, user_id: i64, key: &str) -> Result<Option<String>> {
        self.setting(&user_key(user_id, key))
    }

    pub fn set_user_setting(&self, user_id: i64, key: &str, value: &str) -> Result<()> {
        self.set_setting(&user_key(user_id, key), value)
    }

    pub fn del_user_setting(&self, user_id: i64, key: &str) -> Result<()> {
        self.del_setting(&user_key(user_id, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fresh database gets exactly one profile — nothing to attribute
    /// data to otherwise. Tests key off this rather than assuming id 1.
    fn def_user(db: &Db) -> i64 {
        db.users().unwrap()[0].id
    }

    #[test]
    fn a_fresh_database_has_one_profile_and_no_goals() {
        let db = Db::open_in_memory().unwrap();
        let users = db.users().unwrap();
        assert_eq!(users.len(), 1);
        let u = def_user(&db);
        assert!(db.current_goal(u).unwrap().is_none());
        assert!(db.goals(u).unwrap().is_empty());
    }

    /// The real thing this guards: an existing t-fit.sqlite3 — with a
    /// real goal already set — must come through an upgrade with that
    /// goal intact as history, not silently dropped, and now owned by
    /// the default profile created for it.
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
        let u = def_user(&db);

        let goals = db.goals(u).unwrap();
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

        // Running it again (every later startup) must not duplicate the row,
        // and must not create a second default profile either.
        db.migrate().unwrap();
        assert_eq!(db.goals(u).unwrap().len(), 1);
        assert_eq!(db.users().unwrap().len(), 1);
    }

    /// A t-fit.sqlite3 from just before multi-user (goals table already
    /// exists, no `user_id` column yet, real entries and a real goal on
    /// file) must upgrade with everything intact under one default profile
    /// — this is the exact shape an existing install upgrades from.
    #[test]
    fn migrates_a_pre_multi_user_database_onto_a_default_profile() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE weight (
                day        TEXT PRIMARY KEY,
                weight_lb  REAL NOT NULL,
                memo       TEXT NOT NULL DEFAULT '',
                source     TEXT NOT NULL DEFAULT 'manual',
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE goals (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                target_lb   REAL NOT NULL,
                target_date TEXT,
                start_lb    REAL NOT NULL,
                start_date  TEXT NOT NULL,
                created_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO weight (day, weight_lb, memo, source) VALUES
                ('2026-01-01', 200.0, 'started', 'manual'),
                ('2026-01-02', 199.5, '', 'manual');
            INSERT INTO goals (target_lb, target_date, start_lb, start_date)
                VALUES (180.0, '2026-06-01', 200.0, '2026-01-01');
            INSERT INTO settings (key, value) VALUES
                ('withings.client_id', 'abc123'),
                ('withings.access_token', 'tok'),
                ('withings.refresh_token', 'ref'),
                ('withings.last_sync', '2026-02-01T00:00:00Z');",
        )
        .unwrap();
        let db = Db(Arc::new(Mutex::new(conn)));

        db.migrate().unwrap();
        let u = def_user(&db);

        let entries = db.entries(u).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].memo, "started");

        let goals = db.goals(u).unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].target_lb, 180.0);

        // The whole Withings setup moves onto the profile — registration
        // included, because Withings credentials belong to a person — and
        // nothing is left behind under the old app-wide keys.
        assert_eq!(db.user_setting(u, "withings.client_id").unwrap().as_deref(), Some("abc123"));
        assert_eq!(db.user_setting(u, "withings.access_token").unwrap().as_deref(), Some("tok"));
        assert_eq!(db.user_setting(u, "withings.refresh_token").unwrap().as_deref(), Some("ref"));
        assert!(db.setting("withings.client_id").unwrap().is_none());
        assert!(db.setting("withings.access_token").unwrap().is_none());

        // Idempotent: running it again changes nothing further.
        db.migrate().unwrap();
        assert_eq!(db.users().unwrap().len(), 1);
        assert_eq!(db.entries(u).unwrap().len(), 2);
    }

    #[test]
    fn a_new_goal_becomes_current_and_the_old_one_becomes_history() {
        let db = Db::open_in_memory().unwrap();
        let u = def_user(&db);
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();

        let first = db.add_goal(u, 190.0, None, 210.0, d("2025-06-01")).unwrap();
        let second = db.add_goal(u, 180.0, None, 190.0, d("2026-01-01")).unwrap();

        let current = db.current_goal(u).unwrap().unwrap();
        assert_eq!(current.id, second.id);

        let all = db.goals(u).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, second.id); // newest first
        assert_eq!(all[1].id, first.id);
    }

    #[test]
    fn deleting_a_goal_removes_only_that_one() {
        let db = Db::open_in_memory().unwrap();
        let u = def_user(&db);
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let a = db.add_goal(u, 190.0, None, 210.0, d("2025-06-01")).unwrap();
        let b = db.add_goal(u, 180.0, None, 190.0, d("2026-01-01")).unwrap();

        assert!(db.delete_goal(u, a.id).unwrap());
        let remaining = db.goals(u).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, b.id);
    }

    #[test]
    fn entries_and_goals_are_scoped_to_their_own_profile() {
        let db = Db::open_in_memory().unwrap();
        let a = def_user(&db);
        let b = db.create_user("Partner").unwrap().id;
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();

        db.upsert(a, &Entry { date: d("2026-01-01"), weight_lb: 200.0, memo: String::new(), source: Source::Manual }).unwrap();
        db.upsert(b, &Entry { date: d("2026-01-01"), weight_lb: 140.0, memo: String::new(), source: Source::Manual }).unwrap();
        db.add_goal(a, 180.0, None, 200.0, d("2026-01-01")).unwrap();

        assert_eq!(db.entries(a).unwrap().len(), 1);
        assert_eq!(db.entries(b).unwrap().len(), 1);
        assert_eq!(db.entries(a).unwrap()[0].weight_lb, 200.0);
        assert_eq!(db.entries(b).unwrap()[0].weight_lb, 140.0);
        assert_eq!(db.goals(a).unwrap().len(), 1);
        assert!(db.goals(b).unwrap().is_empty());
    }

    #[test]
    fn cannot_delete_the_last_profile() {
        let db = Db::open_in_memory().unwrap();
        let u = def_user(&db);
        assert!(db.delete_user(u).is_err());
    }

    #[test]
    fn deleting_a_profile_removes_its_data_and_falls_back_the_active_selection() {
        let db = Db::open_in_memory().unwrap();
        let a = def_user(&db);
        let b = db.create_user("Partner").unwrap().id;
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        db.upsert(b, &Entry { date: d("2026-01-01"), weight_lb: 140.0, memo: String::new(), source: Source::Manual }).unwrap();
        db.set_user_setting(b, "withings.access_token", "tok").unwrap();
        db.set_active_user(b).unwrap();

        db.delete_user(b).unwrap();

        assert_eq!(db.users().unwrap().len(), 1);
        assert!(db.user_setting(b, "withings.access_token").unwrap().is_none());
        // The active profile pointed at the one just deleted — it must not
        // be left dangling.
        assert_eq!(db.active_user_id().unwrap(), a);
    }

    #[test]
    fn active_user_falls_back_when_unset_or_stale() {
        let db = Db::open_in_memory().unwrap();
        let a = def_user(&db);
        assert_eq!(db.active_user_id().unwrap(), a);

        let b = db.create_user("Partner").unwrap().id;
        db.set_active_user(b).unwrap();
        assert_eq!(db.active_user_id().unwrap(), b);

        assert!(db.set_active_user(9999).is_err());
        assert_eq!(db.active_user_id().unwrap(), b, "a rejected switch must not change anything");
    }

    #[test]
    fn profile_names_cannot_collide_case_insensitively() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("Alex").unwrap();
        assert!(db.create_user("alex").is_err());
        assert!(db.create_user("  ").unwrap_err().to_string().contains("name"));
    }
}
