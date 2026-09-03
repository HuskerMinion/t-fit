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
use crate::model::{Entry, Source};
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
    db.set_user_setting(uid, key::CLIENT_ID, id)?;
    if !secret.is_empty() {
        db.set_user_setting(uid, key::CLIENT_SECRET, secret)?;
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

/// Pull weight measurements from `since` (default: everything) and add any day
/// we don't already have to this profile's log. Days already in the
/// database are never touched — a hand-typed weight and its note always win.
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
            ("meastype", "1"), // 1 = weight
            ("category", "1"), // 1 = real measurements, not goals
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
    let mut per_day: std::collections::BTreeMap<NaiveDate, (i64, f64)> = Default::default();
    let mut fetched = 0usize;
    for g in &body.measuregrps {
        let Some(m) = g.measures.iter().find(|m| m.kind == 1) else { continue };
        let kg = m.value * 10f64.powi(m.unit);
        if !(kg.is_finite() && kg > 2.0 && kg < 700.0) {
            continue;
        }
        let Some(dt) = DateTime::from_timestamp(g.date, 0) else { continue };
        let local = dt.with_timezone(&chrono::Local);
        let day = local.date_naive();
        fetched += 1;
        per_day
            .entry(day)
            .and_modify(|slot| {
                if g.date < slot.0 {
                    *slot = (g.date, kg * KG_TO_LB);
                }
            })
            .or_insert((g.date, kg * KG_TO_LB));
    }

    let mut inserted = 0usize;
    let mut skipped = 0usize;
    for (day, (_, lb)) in &per_day {
        let e = Entry {
            date: *day,
            weight_lb: (lb * 10.0).round() / 10.0,
            memo: String::new(),
            source: Source::Withings,
        };
        if db.insert_if_absent(uid, &e)? {
            inserted += 1;
        } else {
            skipped += 1;
        }
    }

    db.set_user_setting(uid, key::LAST_SYNC, &Utc::now().to_rfc3339())?;
    db.del_user_setting(uid, key::LAST_ERROR)?;
    Ok(SyncReport {
        fetched,
        inserted,
        skipped_existing: skipped,
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

    #[test]
    fn kg_converts_to_pounds() {
        // Withings sends 82.5 kg as value 82500, unit -3.
        let kg = 82500.0 * 10f64.powi(-3);
        let lb = kg * KG_TO_LB;
        assert!((lb - 181.88).abs() < 0.01, "got {lb}");
    }
}
