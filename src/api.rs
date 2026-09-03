//! HTTP surface. The desktop window and the browser both talk to this, so
//! there is exactly one implementation of everything.

use crate::db::Db;
use crate::import::{self, ImportReport};
use crate::model::{Entry, Goal, Source, Stats};
use crate::stats;
use crate::withings;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::NaiveDate;
use serde::Deserialize;

#[derive(Clone)]
pub struct App {
    pub db: Db,
    /// The origin this server answers on, so the Withings redirect URI we send
    /// matches the one you registered.
    pub base_url: String,
    /// Where the database actually is. Shown in the UI because nobody should
    /// have to read the docs to find their own data.
    pub db_path: String,
}

#[derive(rust_embed::Embed)]
#[folder = "web/"]
struct Web;

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/entries", get(list_entries).post(put_entry))
        .route("/api/entries/:date", axum::routing::delete(del_entry))
        .route("/api/stats", get(get_stats))
        .route("/api/series", get(get_series))
        .route("/api/goal", get(get_goal).put(put_goal))
        .route("/api/import", post(post_import))
        .route("/api/export.csv", get(export_csv))
        .route("/api/withings/status", get(w_status))
        .route("/api/withings/config", post(w_config))
        .route("/api/withings/authorize", get(w_authorize))
        .route("/api/withings/callback", get(w_callback))
        .route("/api/withings/sync", post(w_sync))
        .route("/api/withings/unlink", post(w_unlink))
        .route("/api/withings/clear_error", post(w_clear_error))
        .route("/api/version", get(version))
        .route("/api/prefs", get(get_prefs).put(put_prefs))
        .fallback(static_handler)
        .with_state(app)
}

type ApiResult<T> = Result<T, ApiError>;

pub struct ApiError(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::error!("{:#}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}

async fn list_entries(State(a): State<App>) -> ApiResult<Json<Vec<Entry>>> {
    Ok(Json(a.db.entries()?))
}

#[derive(Deserialize)]
pub struct EntryIn {
    pub date: NaiveDate,
    pub weight_lb: f64,
    #[serde(default)]
    pub memo: String,
}

async fn put_entry(State(a): State<App>, Json(b): Json<EntryIn>) -> ApiResult<Json<Entry>> {
    let e = Entry {
        date: b.date,
        weight_lb: b.weight_lb,
        memo: b.memo,
        source: Source::Manual,
    };
    a.db.upsert(&e)?;
    Ok(Json(e))
}

async fn del_entry(State(a): State<App>, Path(date): Path<String>) -> ApiResult<StatusCode> {
    let d = NaiveDate::parse_from_str(&date, "%Y-%m-%d")?;
    Ok(if a.db.delete(d)? {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
}

async fn get_stats(State(a): State<App>) -> ApiResult<Json<Stats>> {
    let e = a.db.entries()?;
    Ok(Json(stats::compute(&e, a.db.goal()?)))
}

#[derive(Deserialize)]
pub struct SeriesQuery {
    /// Trailing window in days; omit for everything.
    pub days: Option<i64>,
}

#[derive(serde::Serialize)]
pub struct SeriesPoint {
    pub d: String,
    pub w: f64,
    pub t: Option<f64>,
    pub m: bool,
}

async fn get_series(
    State(a): State<App>,
    Query(q): Query<SeriesQuery>,
) -> ApiResult<Json<Vec<SeriesPoint>>> {
    let all = a.db.entries()?;
    let ma: std::collections::HashMap<NaiveDate, f64> =
        stats::moving_average(&all, 7).into_iter().collect();
    let cutoff = match (q.days, all.last()) {
        (Some(d), Some(l)) => Some(l.date - chrono::Duration::days(d)),
        _ => None,
    };
    Ok(Json(
        all.iter()
            .filter(|e| cutoff.map_or(true, |c| e.date >= c))
            .map(|e| SeriesPoint {
                d: e.date.format("%Y-%m-%d").to_string(),
                w: e.weight_lb,
                t: ma.get(&e.date).copied(),
                m: !e.memo.is_empty(),
            })
            .collect(),
    ))
}

async fn get_goal(State(a): State<App>) -> ApiResult<Json<Goal>> {
    Ok(Json(a.db.goal()?))
}

async fn put_goal(State(a): State<App>, Json(g): Json<Goal>) -> ApiResult<Json<Goal>> {
    a.db.set_goal(&g)?;
    Ok(Json(a.db.goal()?))
}

#[derive(Deserialize)]
pub struct ImportQuery {
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub source: Option<String>,
}

async fn post_import(
    State(a): State<App>,
    Query(q): Query<ImportQuery>,
    body: String,
) -> ApiResult<Json<ImportReport>> {
    let src = q.source.as_deref().map(Source::parse).unwrap_or(Source::Import);
    Ok(Json(import::import_csv(&a.db, &body, src, q.overwrite)?))
}

async fn export_csv(State(a): State<App>) -> ApiResult<Response> {
    let mut w = csv::Writer::from_writer(vec![]);
    w.write_record(["date", "weight_lb", "memo", "source"])?;
    for e in a.db.entries()? {
        w.write_record([
            e.date.format("%Y-%m-%d").to_string(),
            format!("{:.2}", e.weight_lb),
            e.memo,
            e.source.as_str().to_string(),
        ])?;
    }
    let body = String::from_utf8(w.into_inner()?)?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"t-fit-export.csv\"",
            ),
        ],
        body,
    )
        .into_response())
}

/// UI preferences, kept as one opaque JSON blob.
///
/// Deliberately schemaless: the front end owns the shape, so adding a new
/// setting later is a change to one file, not four.
async fn get_prefs(State(a): State<App>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(match a.db.setting("ui.prefs")? {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({})),
        None => serde_json::json!({}),
    }))
}

async fn put_prefs(
    State(a): State<App>,
    Json(v): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    a.db.set_setting("ui.prefs", &serde_json::to_string(&v)?)?;
    Ok(Json(v))
}

/// Which build is actually running. An installed copy and a fresh build look
/// identical from the outside, so the UI says it out loud.
async fn version(State(a): State<App>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "built": env!("BUILD_DATE"),
        "db_path": a.db_path,
    }))
}

/* ── Withings ──────────────────────────────────────────────────── */

fn redirect_uri(a: &App) -> String {
    format!("{}/api/withings/callback", a.base_url.trim_end_matches('/'))
}

async fn w_status(State(a): State<App>) -> ApiResult<Json<serde_json::Value>> {
    let s = withings::status(&a.db)?;
    Ok(Json(serde_json::json!({
        "configured": s.configured,
        "client_id": s.client_id,
        "has_secret": s.has_secret,
        "linked": s.linked,
        "last_sync": s.last_sync,
        "last_error": s.last_error,
        "redirect_uri": redirect_uri(&a),
    })))
}

async fn w_config(
    State(a): State<App>,
    Json(c): Json<withings::Config>,
) -> ApiResult<Json<serde_json::Value>> {
    withings::save_config(&a.db, &c)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn w_authorize(State(a): State<App>) -> ApiResult<Json<serde_json::Value>> {
    let url = withings::authorize_url(&a.db, &redirect_uri(&a))?;
    Ok(Json(serde_json::json!({ "url": url })))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// Withings sends the browser here. Answer with a small page rather than JSON —
/// a human is looking at it.
async fn w_callback(State(a): State<App>, Query(q): Query<CallbackQuery>) -> Response {
    let outcome = match (q.error, q.code, q.state) {
        (Some(e), _, _) => Err(format!("Withings declined the link: {e}")),
        (_, Some(code), Some(state)) => {
            withings::exchange_code(&a.db, &code, &state, &redirect_uri(&a))
                .await
                .map_err(|e| format!("{e:#}"))
        }
        _ => Err("Withings sent us back without an authorization code.".to_string()),
    };
    if let Err(ref e) = outcome {
        withings::record_error(&a.db, e);
    }

    let ok = outcome.is_ok();
    let title = if ok { "Withings linked" } else { "Could not link Withings" };
    let detail = match &outcome {
        Ok(()) => "Pulling your weigh-ins now. This tab closes itself.".to_string(),
        Err(e) => html_escape(e),
    };
    // Tell the t-fit window what happened, then get out of the way if it
    // worked. On failure the tab stays put so the reason can be read.
    let html = format!(
        r#"<!doctype html><meta charset="utf-8"><title>{title}</title>
<style>
 body{{font:15px/1.55 ui-sans-serif,system-ui,"Segoe UI",sans-serif;background:#1e2124;color:#eaecef;
      display:grid;place-items:center;min-height:100vh;margin:0;text-align:center;padding:24px}}
 h1{{font-size:19px;margin:0 0 10px}} p{{color:#b0b7be;max-width:40em;margin:0 auto}}
 .bad{{color:#e2645f}}
</style>
<div><h1 class="{cls}">{title}</h1><p>{detail}</p></div>
<script>
  try {{ if (window.opener) window.opener.postMessage({{ tfit: "withings", ok: {ok} }}, "*"); }} catch (e) {{}}
  if ({ok}) setTimeout(function () {{ window.close(); }}, 1500);
</script>"#,
        cls = if ok { "" } else { "bad" },
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[derive(Deserialize)]
pub struct SyncQuery {
    /// Only pull days on or after this one. Omit for everything Withings has.
    pub since: Option<NaiveDate>,
}

async fn w_sync(
    State(a): State<App>,
    Query(q): Query<SyncQuery>,
) -> ApiResult<Json<withings::SyncReport>> {
    match withings::sync(&a.db, q.since).await {
        Ok(r) => Ok(Json(r)),
        Err(e) => {
            withings::record_error(&a.db, &format!("{e:#}"));
            Err(e.into())
        }
    }
}

async fn w_unlink(State(a): State<App>) -> ApiResult<Json<serde_json::Value>> {
    withings::unlink(&a.db)?;
    withings::clear_error(&a.db)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn w_clear_error(State(a): State<App>) -> ApiResult<Json<serde_json::Value>> {
    withings::clear_error(&a.db)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn static_handler(uri: Uri) -> Response {
    let mut p = uri.path().trim_start_matches('/').to_string();
    if p.is_empty() {
        p = "index.html".into();
    }
    match Web::get(&p) {
        Some(f) => {
            // mime_guess doesn't know .webmanifest, and browsers are picky
            // about the manifest's content type.
            let mime = if p.ends_with(".webmanifest") {
                "application/manifest+json".to_string()
            } else {
                mime_guess::from_path(&p).first_or_octet_stream().to_string()
            };
            ([(header::CONTENT_TYPE, mime)], f.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
