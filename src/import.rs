//! Bulk import. Deliberately tolerant about date and header spelling, because
//! the files people actually have are never quite the shape you expect.

use crate::db::Db;
use crate::model::{Entry, Source};
use anyhow::{anyhow, Result};
use chrono::NaiveDate;

#[derive(Debug, Default, serde::Serialize)]
pub struct ImportReport {
    pub read: usize,
    pub inserted: usize,
    pub skipped_existing: usize,
    pub errors: Vec<String>,
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    for f in ["%Y-%m-%d", "%m/%d/%Y", "%d/%m/%Y", "%Y/%m/%d", "%m-%d-%Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return Some(d);
        }
    }
    // Withings exports timestamps like "2026-09-02 02:21:51"
    if s.len() >= 10 {
        if let Ok(d) = NaiveDate::parse_from_str(&s[..10], "%Y-%m-%d") {
            return Some(d);
        }
    }
    None
}

fn find(headers: &csv::StringRecord, names: &[&str]) -> Option<usize> {
    headers.iter().position(|h| {
        let h = h.trim().to_ascii_lowercase();
        names.iter().any(|n| h == *n || h.starts_with(n))
    })
}

/// Import a CSV into one profile's log. Recognises both our own export and
/// a raw Withings `weight.csv`. When a day appears more than once, the
/// earliest reading of that day wins — that's the morning weigh-in, which
/// is the comparable one.
pub fn import_csv(
    db: &Db,
    user_id: i64,
    data: &str,
    source: Source,
    overwrite: bool,
) -> Result<ImportReport> {
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(data.as_bytes());
    let headers = rdr.headers()?.clone();

    let di = find(&headers, &["date", "day"]).ok_or_else(|| anyhow!("no date column"))?;
    let wi = find(&headers, &["weight"]).ok_or_else(|| anyhow!("no weight column"))?;
    let mi = find(&headers, &["memo", "note", "comment"]);

    let mut rep = ImportReport::default();
    // day -> (raw order index, entry) so we can keep the first of a day
    let mut best: std::collections::BTreeMap<NaiveDate, Entry> = Default::default();

    for (n, rec) in rdr.records().enumerate() {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                rep.errors.push(format!("row {}: {e}", n + 2));
                continue;
            }
        };
        rep.read += 1;
        let Some(date) = rec.get(di).and_then(parse_date) else {
            rep.errors.push(format!("row {}: unreadable date", n + 2));
            continue;
        };
        let Some(w) = rec
            .get(wi)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|w| *w > 0.0 && *w < 2000.0)
        else {
            rep.errors.push(format!("row {}: unreadable weight", n + 2));
            continue;
        };
        let memo = mi
            .and_then(|i| rec.get(i))
            .unwrap_or("")
            .trim()
            .to_string();
        best.entry(date).or_insert(Entry {
            date,
            weight_lb: w,
            memo,
            source,
        });
    }

    for e in best.values() {
        let changed = if overwrite {
            db.upsert(user_id, e)?
        } else {
            db.insert_if_absent(user_id, e)?
        };
        if changed {
            rep.inserted += 1;
        } else {
            rep.skipped_existing += 1;
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def_user(db: &Db) -> i64 {
        db.users().unwrap()[0].id
    }

    #[test]
    fn imports_our_own_export_with_memos() {
        let db = Db::open_in_memory().unwrap();
        let u = def_user(&db);
        let csv = "date,weight_lb,memo\n2011-11-08,260.0,started Atkins\n2011-11-09,260.0,\n";
        let r = import_csv(&db, u, csv, Source::Fitday, true).unwrap();
        assert_eq!(r.inserted, 2);
        let all = db.entries(u).unwrap();
        assert_eq!(all[0].memo, "started Atkins");
    }

    #[test]
    fn imports_a_raw_withings_export_and_keeps_the_first_reading_of_a_day() {
        let db = Db::open_in_memory().unwrap();
        let u = def_user(&db);
        let csv = "Date,\"Weight (lb)\",\"Fat mass (lb)\"\n\
                   \"2026-09-01 06:15:02\",268.9,105\n\
                   \"2026-09-01 21:40:00\",271.4,105\n";
        let r = import_csv(&db, u, csv, Source::Withings, true).unwrap();
        assert_eq!(r.inserted, 1);
        assert!((db.entries(u).unwrap()[0].weight_lb - 268.9).abs() < 1e-9);
    }

    #[test]
    fn sync_style_import_never_clobbers_a_hand_typed_day() {
        let db = Db::open_in_memory().unwrap();
        let u = def_user(&db);
        import_csv(&db, u, "date,weight_lb,memo\n2026-09-01,268.9,mine\n", Source::Manual, true).unwrap();
        import_csv(&db, u, "date,weight_lb\n2026-09-01,999.0\n", Source::Withings, false).unwrap();
        let all = db.entries(u).unwrap();
        assert!((all[0].weight_lb - 268.9).abs() < 1e-9);
        assert_eq!(all[0].memo, "mine");
    }

    #[test]
    fn two_profiles_importing_the_same_day_do_not_collide() {
        let db = Db::open_in_memory().unwrap();
        let a = def_user(&db);
        let b = db.create_user("Partner").unwrap().id;
        import_csv(&db, a, "date,weight_lb\n2026-09-01,200.0\n", Source::Manual, true).unwrap();
        import_csv(&db, b, "date,weight_lb\n2026-09-01,140.0\n", Source::Manual, true).unwrap();
        assert!((db.entries(a).unwrap()[0].weight_lb - 200.0).abs() < 1e-9);
        assert!((db.entries(b).unwrap()[0].weight_lb - 140.0).abs() < 1e-9);
    }
}
