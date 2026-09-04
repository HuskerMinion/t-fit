//! Withings sync.
//!
//! Withings speaks OAuth2 with a non-standard token endpoint (everything is a
//! form POST to `/v2/oauth2` with an `action` field, and errors come back as
//! HTTP 200 with a non-zero `status`). This module hides that.
//!
//! Nothing here is a secret in the repo sense: your client id and secret come
//! from your own free developer app and are stored in the local SQLite
//! `settings` table, never in the source tree.
//!
//! Everything Withings-related is per profile, registration included. Withings
//! hands out credentials per person, not per app: two people linking their own
//! accounts each need their own developer app, so each profile keeps its own
//! client id and secret alongside its own tokens.

use crate::db::Db;
use crate::model::{Composition, Entry, Source};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

const AUTH_URL: &str = "https://account.withings.com/oauth2_user/authorize2";
const OAUTH_URL: &str = "https://wbsapi.withings.net/v2/oauth2";
const MEASURE_URL: &str = "https://wbsapi.withings.net/measure";
const KG_TO_LB: f64 = 2.204_622_621_848_776;

/// Settings keys. Kept in one place so nothing drifts.
mod key {
    pub const CLIENT_ID: &str = "withings.client_id";
    pub const CLIENT_SECRET: &str = "withings.client_secret";
    pub const ACCESS: &str = "withings.access_token";
    pub const REFRESH: &str = "withings.refresh_token";
    pub const EXPIRES: &str = "withings.expires_at";
    pub const STATE: &str = "withings.oauth_state";
    pub const LAST_SYNC: &str = "withings.last_sync";
    /// Whatever went wrong last, kept so the main window can show it even
    /// after the callback tab has been closed.
    pub const LAST_ERROR: &str = "withings.last_error";
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// A client id and secret have been saved.
    pub configured: bool,
    /// The saved client id. Not a secret — it travels in the authorize URL —
    /// and showing it back is what makes "did that save?" answerable.
    pub client_id: Option<String>,
    /// Whether a secret is on file. The secret itself is never sent back.
    pub has_secret: bool,
    /// We hold tokens, so a sync will work.
    pub linked: bool,
    pub last_sync: Option<String>,
    /// The last failure, if the last thing that happened was a failure.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub fetched: usize,
    pub inserted: usize,
    pub skipped_existing: usize,
    /// Days that already existed and gained body composition they didn't
    /// have. Counted separately from `inserted` because it's the number
    /// that explains a backfill over years of history.
    pub enriched: usize,
    pub first: Option<String>,
    pub last: Option<String>,
}

/// Status is always asked for on behalf of one profile — registration and
/// link alike belong to that person, so a profile with nothing saved reads
/// as "not configured" no matter what anyone else has set up.
pub fn status(db: &Db, uid: i64) -> Result<Status> {
    let client_id = db.user_setting(uid, key::CLIENT_ID)?;
    let has_secret = db.user_setting(uid, key::CLIENT_SECRET)?.is_some();
    Ok(Status {
        configured: client_id.is_some() && has_secret,
        client_id,
        has_secret,
        linked: db.user_setting(uid, key::REFRESH)?.is_some(),
        last_sync: db.user_setting(uid, key::LAST_SYNC)?,
        last_error: db.user_setting(uid, key::LAST_ERROR)?,
    })
}

/// Remember why something failed. The OAuth callback happens in a tab the
/// user may close before reading it, so the reason has to outlive that tab.
pub fn record_error(db: &Db, uid: i64, msg: &str) {
    let _ = db.set_user_setting(uid, key::LAST_ERROR, msg);
}

pub fn clear_error(db: &Db, uid: i64) -> Result<()> {
    db.del_user_setting(uid, key::LAST_ERROR)
}

/// Save one profile's registration. An empty secret means "keep the one
/// already stored", so the client id can be corrected without retyping the
/// secret — and so a blank box never silently wipes a working setup.
pub fn save_config(db: &Db, uid: i64, c: &Config) -> Result<()> {
    let id = c.client_id.trim();
    let secret = c.client_secret.trim();
    if id.is_empty() {
        bail!("a client id is required");
    }
    if secret.is_empty() && db.user_setting(uid, key::CLIENT_SECRET)?.is_none() {
        bail!("a client secret is required the first time");
    }
    let previous = db.user_setting(uid, key::CLIENT_ID)?;
    db.set_user_setting(uid, key::CLIENT_ID, id)?;
    if !secret.is_empty() {
        db.set_user_setting(uid, key::CLIENT_SECRET, secret)?;
    }
    // Tokens belong to the application that minted them: Withings won't
    // refresh a token presented with a different client id. So pointing a
    // profile at a new registration makes whatever it is holding dead
    // weight — it would keep syncing off the cached access token and then
    // fail hours later, looking for all the world like a Withings outage.
    // Drop it here instead, so the card plainly says "connect" while the
    // person is still sitting in front of it.
    if previous.as_deref() != Some(id) {
        unlink(db, uid)?;
    }
    Ok(())
}

/// Forget this profile's tokens (but keep its saved app registration, so
/// reconnecting doesn't mean retyping the client id and secret).
pub fn unlink(db: &Db, uid: i64) -> Result<()> {
    for k in [key::ACCESS, key::REFRESH, key::EXPIRES, key::STATE] {
        db.del_user_setting(uid, k)?;
    }
    Ok(())
}

/// The URL to send the browser to. `redirect_uri` must match the one
/// registered with Withings exactly.
///
/// The state Withings echoes back is our only way to know, once the
/// callback lands, which profile started this — the tab it opens in isn't
/// necessarily still showing the same "active" user by then. So `uid`
/// travels inside the state string itself rather than being looked up
/// fresh at callback time.
pub fn authorize_url(db: &Db, uid: i64, redirect_uri: &str) -> Result<String> {
    let id = db
        .user_setting(uid, key::CLIENT_ID)?
        .ok_or_else(|| anyhow!("Withings is not configured yet — save a client id and secret first"))?;
    let state = format!("{uid}:{:x}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    db.set_user_setting(uid, key::STATE, &state)?;
    Ok(format!(
        "{AUTH_URL}?response_type=code&client_id={}&scope=user.metrics&state={}&redirect_uri={}",
        urlencoding::encode(&id),
        urlencoding::encode(&state),
        urlencoding::encode(redirect_uri),
    ))
}

/// Pull the profile id back out of a state string built by `authorize_url`.
/// Used by the callback to know whose tokens to store, and to attribute an
/// error, before anything has been validated yet.
pub fn uid_from_state(state: &str) -> Option<i64> {
    state.split(':').next()?.parse().ok()
}

/// Withings wraps every reply: `{"status":0,"body":{...}}`, and reports
/// failures with HTTP 200 and a non-zero status.
#[derive(Deserialize)]
struct Envelope<T> {
    status: i64,
    #[serde(default)]
    error: Option<String>,
    body: Option<T>,
}

fn unwrap_envelope<T>(e: Envelope<T>, what: &str) -> Result<T> {
    if e.status != 0 {
        bail!(
            "Withings {what} failed (status {}): {}",
            e.status,
            e.error.unwrap_or_else(|| "no detail given".into())
        );
    }
    e.body.ok_or_else(|| anyhow!("Withings {what} returned no body"))
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

fn store_tokens(db: &Db, uid: i64, t: &Tokens) -> Result<()> {
    db.set_user_setting(uid, key::ACCESS, &t.access_token)?;
    db.set_user_setting(uid, key::REFRESH, &t.refresh_token)?;
    // Expire a minute early so a sync never races the deadline.
    let at = Utc::now() + Duration::seconds(t.expires_in.saturating_sub(60).max(0));
    db.set_user_setting(uid, key::EXPIRES, &at.to_rfc3339())?;
    Ok(())
}

/// This profile's own client id and secret. Whose credentials get used has
/// to follow whose tokens are being minted or refreshed — mixing the two is
/// what Withings rejects as a `redirect_uri_mismatch`.
fn creds(db: &Db, uid: i64) -> Result<(String, String)> {
    Ok((
        db.user_setting(uid, key::CLIENT_ID)?
            .ok_or_else(|| anyhow!("no Withings client id saved for this profile"))?,
        db.user_setting(uid, key::CLIENT_SECRET)?
            .ok_or_else(|| anyhow!("no Withings client secret saved for this profile"))?,
    ))
}

/// Exchange the `code` from the redirect for tokens. Which profile these
/// belong to comes from `state` itself (see `authorize_url`), not from
/// whatever happens to be "active" right now.
pub async fn exchange_code(db: &Db, code: &str, state: &str, redirect_uri: &str) -> Result<()> {
    let uid = uid_from_state(state)
        .ok_or_else(|| anyhow!("Withings sent back a state we don't recognize — start the link again from t-fit"))?;
    let expect = db.user_setting(uid, key::STATE)?.unwrap_or_default();
    if expect.is_empty() || expect != state {
        bail!("OAuth state did not match — start the link again from t-fit");
    }
    let (id, secret) = creds(db, uid)?;
    let res: Envelope<Tokens> = reqwest::Client::new()
        .post(OAUTH_URL)
        .form(&[
            ("action", "requesttoken"),
            ("grant_type", "authorization_code"),
            ("client_id", &id),
            ("client_secret", &secret),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await
        .context("could not reach Withings")?
        .json()
        .await
        .context("Withings sent something that wasn't JSON")?;
    store_tokens(db, uid, &unwrap_envelope(res, "token exchange")?)?;
    db.del_user_setting(uid, key::STATE)?;
    db.del_user_setting(uid, key::LAST_ERROR)?;
    Ok(())
}

/// A valid access token for this profile, refreshing first if it's stale.
async fn access_token(db: &Db, uid: i64) -> Result<String> {
    let expires = db
        .user_setting(uid, key::EXPIRES)?
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc));
    if let (Some(tok), Some(exp)) = (db.user_setting(uid, key::ACCESS)?, expires) {
        if exp > Utc::now() {
            return Ok(tok);
        }
    }
    let refresh = db
        .user_setting(uid, key::REFRESH)?
        .ok_or_else(|| anyhow!("Withings isn't linked yet"))?;
    let (id, secret) = creds(db, uid)?;
    let res: Envelope<Tokens> = reqwest::Client::new()
        .post(OAUTH_URL)
        .form(&[
            ("action", "requesttoken"),
            ("grant_type", "refresh_token"),
            ("client_id", &id),
            ("client_secret", &secret),
            ("refresh_token", &refresh),
        ])
        .send()
        .await
        .context("could not reach Withings")?
        .json()
        .await?;
    let t = unwrap_envelope(res, "token refresh")?;
    store_tokens(db, uid, &t)?;
    Ok(t.access_token)
}

#[derive(Debug, Deserialize)]
struct Measures {
    measuregrps: Vec<Group>,
}
#[derive(Debug, Deserialize)]
struct Group {
    date: i64,
    #[serde(default)]
    measures: Vec<Measure>,
}
#[derive(Debug, Deserialize)]
struct Measure {
    value: f64,
    unit: i32,
    #[serde(rename = "type")]
    kind: i32,
}

/// Withings measurement types. All of these come back from one `getmeas`
/// call — `meastypes` takes a comma-separated list — on the `user.metrics`
/// scope we already hold, so body composition costs no extra request and no
/// re-authorization.
mod meas {
    pub const WEIGHT: i32 = 1;
    pub const FAT_RATIO: i32 = 6;
    pub const MUSCLE: i32 = 76;
    pub const HYDRATION: i32 = 77;
    pub const BONE: i32 = 88;
}

const MEASTYPES: &str = "1,6,76,77,88";

/// Turn one measure into a real value: Withings sends an integer plus a
/// power-of-ten exponent, so 82.5 kg arrives as 82500 with unit -3.
fn scaled(m: &Measure) -> f64 {
    m.value * 10f64.powi(m.unit)
}

/// Everything one weigh-in reported. A group without a plausible weight
/// isn't a weigh-in at all, so `from_group` gives back `None` for it.
struct Reading {
    weight_lb: f64,
    body: Composition,
}

fn from_group(g: &Group) -> Option<Reading> {
    let kg = g.measures.iter().find(|m| m.kind == meas::WEIGHT).map(scaled)?;
    if !(kg.is_finite() && kg > 2.0 && kg < 700.0) {
        return None;
    }
    let lb = |kind: i32| {
        g.measures
            .iter()
            .find(|m| m.kind == kind)
            .map(|m| round1(scaled(m) * KG_TO_LB))
            .filter(|v| v.is_finite() && *v > 0.0)
    };
    let pct = g
        .measures
        .iter()
        .find(|m| m.kind == meas::FAT_RATIO)
        .map(scaled)
        .filter(|v| v.is_finite() && (1.0..=80.0).contains(v))
        .map(round1);
    Some(Reading {
        weight_lb: round1(kg * KG_TO_LB),
        body: Composition {
            fat_ratio: pct,
            muscle_lb: lb(meas::MUSCLE),
            bone_lb: lb(meas::BONE),
            water_lb: lb(meas::HYDRATION),
        },
    })
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Pull measurements from `since` (default: everything) and add any day we
/// don't already have to this profile's log.
///
/// A day already in the database never has its weight, note or source
/// touched — hand-typed always wins. It can still *gain* body composition it
/// was missing, because that's adding a fact rather than overwriting one;
/// see `Db::fill_composition`.
pub async fn sync(db: &Db, uid: i64, since: Option<NaiveDate>) -> Result<SyncReport> {
    let token = access_token(db, uid).await?;
    let start = since
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0)
        .to_string();
    let end = Utc::now().timestamp().to_string();

    let res: Envelope<Measures> = reqwest::Client::new()
        .post(MEASURE_URL)
        .bearer_auth(&token)
        .form(&[
            ("action", "getmeas"),
            ("meastypes", MEASTYPES), // weight plus body composition
            ("category", "1"),        // 1 = real measurements, not goals
            ("startdate", &start),
            ("enddate", &end),
        ])
        .send()
        .await
        .context("could not reach Withings")?
        .json()
        .await?;
    let body = unwrap_envelope(res, "measurement fetch")?;

    // Several readings can land on one day; keep the earliest — the morning
    // weigh-in, which is the one that's actually comparable day to day.
    // Composition comes from that same group rather than being merged across
    // the day's readings, so the numbers on a row all describe one moment on
    // the scale instead of an average of several.
    let mut per_day: std::collections::BTreeMap<NaiveDate, (i64, Reading)> = Default::default();
    let mut fetched = 0usize;
    for g in &body.measuregrps {
        let Some(reading) = from_group(g) else { continue };
        let Some(dt) = DateTime::from_timestamp(g.date, 0) else { continue };
        let day = dt.with_timezone(&chrono::Local).date_naive();
        fetched += 1;
        match per_day.get(&day) {
            Some((at, _)) if *at <= g.date => {}
            _ => {
                per_day.insert(day, (g.date, reading));
            }
        }
    }

    let mut inserted = 0usize;
    let mut skipped = 0usize;
    let mut enriched = 0usize;
    for (day, (_, reading)) in &per_day {
        let e = Entry {
            date: *day,
            weight_lb: reading.weight_lb,
            memo: String::new(),
            source: Source::Withings,
            body: reading.body,
        };
        if db.insert_if_absent(uid, &e)? {
            inserted += 1;
        } else {
            skipped += 1;
            // The day was already there. Its weight and note stay exactly as
            // they are; only composition it was missing gets filled in.
            if db.fill_composition(uid, *day, &reading.body)? {
                enriched += 1;
            }
        }
    }

    db.set_user_setting(uid, key::LAST_SYNC, &Utc::now().to_rfc3339())?;
    db.del_user_setting(uid, key::LAST_ERROR)?;
    Ok(SyncReport {
        fetched,
        inserted,
        skipped_existing: skipped,
        enriched,
        first: per_day.keys().next().map(|d| d.to_string()),
        last: per_day.keys().next_back().map(|d| d.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_reports_withings_errors() {
        let e: Envelope<Measures> =
            serde_json::from_str(r#"{"status":401,"error":"invalid_token"}"#).unwrap();
        let err = unwrap_envelope(e, "test").unwrap_err().to_string();
        assert!(err.contains("401") && err.contains("invalid_token"), "{err}");
    }

    /// The bug this guards against: one shared registration meant the second
    /// person to link overwrote the first person's client id and secret, and
    /// then authorized against credentials that weren't theirs — which is
    /// what Withings bounces as `redirect_uri_mismatch`.
    #[test]
    fn each_profile_keeps_its_own_registration() {
        let db = Db::open_in_memory().unwrap();
        let a = db.users().unwrap()[0].id;
        let b = db.create_user("Wife").unwrap().id;

        save_config(&db, a, &Config { client_id: "id-a".into(), client_secret: "sec-a".into() }).unwrap();
        save_config(&db, b, &Config { client_id: "id-b".into(), client_secret: "sec-b".into() }).unwrap();

        assert_eq!(creds(&db, a).unwrap(), ("id-a".into(), "sec-a".into()));
        assert_eq!(creds(&db, b).unwrap(), ("id-b".into(), "sec-b".into()));
        assert!(status(&db, a).unwrap().configured);
        assert_eq!(status(&db, a).unwrap().client_id.as_deref(), Some("id-a"));

        // The authorize URL has to carry *this* profile's client id.
        let url = authorize_url(&db, b, "https://example.test/cb").unwrap();
        assert!(url.contains("client_id=id-b"), "{url}");

        // An empty secret still means "keep mine" — and doesn't reach across.
        save_config(&db, b, &Config { client_id: "id-b2".into(), client_secret: String::new() }).unwrap();
        assert_eq!(creds(&db, b).unwrap(), ("id-b2".into(), "sec-b".into()));
        assert_eq!(creds(&db, a).unwrap(), ("id-a".into(), "sec-a".into()));
    }

    /// The state that bit us in the field: a profile holding tokens minted
    /// by someone else's application. It syncs off the cached access token
    /// and only fails hours later, at the first refresh. Re-registering has
    /// to clear it rather than leave the trap armed.
    #[test]
    fn pointing_a_profile_at_a_new_registration_drops_its_stale_tokens() {
        let db = Db::open_in_memory().unwrap();
        let u = db.users().unwrap()[0].id;
        save_config(&db, u, &Config { client_id: "old-app".into(), client_secret: "old-sec".into() }).unwrap();
        db.set_user_setting(u, key::ACCESS, "tok").unwrap();
        db.set_user_setting(u, key::REFRESH, "ref").unwrap();
        db.set_user_setting(u, key::EXPIRES, "2030-01-01T00:00:00+00:00").unwrap();
        assert!(status(&db, u).unwrap().linked);

        // Same id, new secret: the link still belongs to this app, so keep it.
        save_config(&db, u, &Config { client_id: "old-app".into(), client_secret: "new-sec".into() }).unwrap();
        assert!(status(&db, u).unwrap().linked, "a secret correction must not unlink");

        // Different id: those tokens can never be refreshed again.
        save_config(&db, u, &Config { client_id: "new-app".into(), client_secret: String::new() }).unwrap();
        let s = status(&db, u).unwrap();
        assert!(!s.linked, "stale tokens should have been dropped");
        assert!(s.configured, "the new registration stays saved");
        assert!(db.user_setting(u, key::ACCESS).unwrap().is_none());
    }

    /// A brand-new profile starts from nothing, rather than inheriting
    /// whoever set up Withings first.
    #[test]
    fn a_new_profile_is_not_configured() {
        let db = Db::open_in_memory().unwrap();
        let a = db.users().unwrap()[0].id;
        save_config(&db, a, &Config { client_id: "id-a".into(), client_secret: "sec-a".into() }).unwrap();

        let b = db.create_user("Wife").unwrap().id;
        let s = status(&db, b).unwrap();
        assert!(!s.configured && !s.has_secret && s.client_id.is_none());
        assert!(creds(&db, b).is_err());
    }

    /// One weigh-in arrives as a group of measures sharing a timestamp.
    /// Everything is an integer plus a power-of-ten exponent, and each metric
    /// can be missing on its own.
    #[test]
    fn a_measure_group_yields_weight_and_whatever_else_was_measured() {
        let g = Group {
            date: 1_700_000_000,
            measures: vec![
                Measure { value: 82500.0, unit: -3, kind: 1 },   // 82.5 kg
                Measure { value: 283.0, unit: -1, kind: 6 },     // 28.3 %
                Measure { value: 58200.0, unit: -3, kind: 76 },  // 58.2 kg muscle
                Measure { value: 3100.0, unit: -3, kind: 88 },   // 3.1 kg bone
            ],
        };
        let r = from_group(&g).expect("a group with a weight is a weigh-in");
        assert!((r.weight_lb - 181.9).abs() < 0.05, "{}", r.weight_lb);
        assert_eq!(r.body.fat_ratio, Some(28.3));
        assert!((r.body.muscle_lb.unwrap() - 128.3).abs() < 0.1, "{:?}", r.body.muscle_lb);
        assert!((r.body.bone_lb.unwrap() - 6.8).abs() < 0.1, "{:?}", r.body.bone_lb);
        assert_eq!(r.body.water_lb, None, "hydration wasn't measured, so it stays absent");
    }

    /// A basic scale sends a weight and nothing else — that must still be a
    /// perfectly good weigh-in, not a group we throw away.
    #[test]
    fn a_weight_with_no_composition_is_still_a_reading() {
        let g = Group {
            date: 1_700_000_000,
            measures: vec![Measure { value: 90000.0, unit: -3, kind: 1 }],
        };
        let r = from_group(&g).unwrap();
        assert!(r.body.is_empty());
        assert!((r.weight_lb - 198.4).abs() < 0.05, "{}", r.weight_lb);
    }

    /// Composition without a weight isn't a weigh-in, and a nonsense weight
    /// is not one either.
    #[test]
    fn groups_without_a_plausible_weight_are_ignored() {
        let no_weight = Group {
            date: 1,
            measures: vec![Measure { value: 283.0, unit: -1, kind: 6 }],
        };
        assert!(from_group(&no_weight).is_none());

        let absurd = Group {
            date: 1,
            measures: vec![Measure { value: 900000.0, unit: -3, kind: 1 }], // 900 kg
        };
        assert!(from_group(&absurd).is_none());
    }

    /// A fat ratio outside anything a human body reports is a bad read, and
    /// a bad read is worse than no read — it would draw a cliff on the chart.
    #[test]
    fn an_impossible_fat_ratio_is_dropped_not_stored() {
        let g = Group {
            date: 1,
            measures: vec![
                Measure { value: 82500.0, unit: -3, kind: 1 },
                Measure { value: 0.0, unit: 0, kind: 6 },
            ],
        };
        assert_eq!(from_group(&g).unwrap().body.fat_ratio, None);
    }

    #[test]
    fn kg_converts_to_pounds() {
        // Withings sends 82.5 kg as value 82500, unit -3.
        let kg = 82500.0 * 10f64.powi(-3);
        let lb = kg * KG_TO_LB;
        assert!((lb - 181.88).abs() < 0.01, "got {lb}");
    }
}
