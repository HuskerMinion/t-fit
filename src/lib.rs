//! t-fit as a library, so both the CLI and the desktop shell start the same
//! server rather than two subtly different ones.

pub mod api;
pub mod db;
pub mod fitday;
pub mod import;
pub mod model;
pub mod stats;
pub mod withings;

use anyhow::Result;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Where the database lives when nobody says otherwise.
pub fn default_db_path() -> PathBuf {
    directories::ProjectDirs::from("dev", "t-fit", "t-fit")
        .map(|d| d.data_dir().join("t-fit.sqlite3"))
        .unwrap_or_else(|| PathBuf::from("t-fit.sqlite3"))
}

/// Bind first, so the caller can learn the real port before the server runs.
/// Passing port 0 asks the OS for a free one — that's how the desktop shell
/// avoids fighting with anything already on 8787.
pub async fn bind(addr: SocketAddr) -> Result<(tokio::net::TcpListener, SocketAddr)> {
    let l = tokio::net::TcpListener::bind(addr).await?;
    let actual = l.local_addr()?;
    Ok((l, actual))
}

/// Serve until Ctrl-C.
pub async fn serve(listener: tokio::net::TcpListener, db_path: &Path, base_url: String) -> Result<()> {
    let db = db::Db::open(db_path)?;
    tracing::info!("database: {}", db_path.display());
    spawn_auto_sync(db.clone());
    let app = api::router(api::App {
        db,
        base_url,
        db_path: db_path.display().to_string(),
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Pull from Withings shortly after start, then every six hours, so a
/// weigh-in shows up without anyone pressing a button. Does nothing at all
/// until Withings has been linked, and never overwrites an existing day.
fn spawn_auto_sync(db: db::Db) {
    tokio::spawn(async move {
        // Wake often, sync rarely. Checking every quarter hour means a
        // changed interval takes effect without a restart, while the actual
        // sync cadence is governed by when we last succeeded.
        const TICK: std::time::Duration = std::time::Duration::from_secs(15 * 60);
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        loop {
            if let Err(e) = auto_sync_tick(&db).await {
                tracing::warn!("auto-sync tick failed: {e:#}");
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// How often to sync, in hours. 0 disables it. Read fresh each tick so the
/// setting applies without a restart.
fn sync_interval_hours(db: &db::Db) -> i64 {
    let raw = match db.setting("ui.prefs") {
        Ok(Some(r)) => r,
        _ => return 6,
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("sync_hours").and_then(|h| h.as_i64()))
        .unwrap_or(6)
        .clamp(0, 24 * 7)
}

async fn auto_sync_tick(db: &db::Db) -> anyhow::Result<()> {
    let hours = sync_interval_hours(db);
    if hours == 0 {
        return Ok(()); // automatic sync switched off
    }
    let status = withings::status(db)?;
    if !status.linked {
        return Ok(());
    }

    // Due when we've never synced, or the last success is older than the
    // interval. Keyed off the stored timestamp rather than a timer, so a
    // restart doesn't trigger a needless pull.
    let due = match status.last_sync.as_deref() {
        None => true,
        Some(t) => match chrono::DateTime::parse_from_rfc3339(t) {
            Ok(when) => {
                chrono::Utc::now() - when.with_timezone(&chrono::Utc)
                    >= chrono::Duration::hours(hours)
            }
            Err(_) => true,
        },
    };
    if !due {
        return Ok(());
    }

    match withings::sync(db, None).await {
        Ok(r) => tracing::info!(
            "withings auto-sync: {} new, {} already present",
            r.inserted,
            r.skipped_existing
        ),
        Err(e) => {
            tracing::warn!("withings auto-sync failed: {e:#}");
            withings::record_error(db, &format!("{e:#}"));
        }
    }
    Ok(())
}

/// Best-effort LAN address, so the startup banner prints something you can
/// actually type into a phone.
pub fn local_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

/// Open `url` as an app window.
///
/// "App window" means a Chromium browser launched with `--app=`, which gives
/// a chromeless window with no tabs or address bar. That only works with
/// Chromium-family browsers, so the rule is: use the browser the person
/// actually chose as their default, and only reach past it when it can't do
/// the job.
pub fn open_app_window(url: &str, browser: Option<&Path>) {
    // 1. An explicit --browser wins over everything.
    if let Some(p) = browser {
        if launch_app_mode(p, url) {
            return;
        }
        tracing::warn!("could not launch {}; falling back", p.display());
    }

    // 2. The default browser. If it's Chromium-family, it gets the app
    //    window. If it isn't (Firefox, say), open a normal tab in it rather
    //    than dragging in some other browser the person didn't pick.
    if let Some(exe) = default_browser_exe() {
        if is_chromium_family(&exe) {
            if launch_app_mode(&exe, url) {
                return;
            }
        } else {
            tracing::info!(
                "default browser {} has no app mode; opening a normal tab",
                exe.display()
            );
            let _ = open::that_detached(url);
            return;
        }
    }

    // 3. No usable default: try any Chromium-family browser that's installed.
    for exe in INSTALLED_CANDIDATES {
        if Path::new(exe).exists() && launch_app_mode(Path::new(exe), url) {
            return;
        }
    }

    // 4. Give up on the app window and just open the URL however the system
    //    normally would.
    let _ = open::that_detached(url);
}

#[cfg(target_os = "windows")]
const INSTALLED_CANDIDATES: &[&str] = &[
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
];
#[cfg(target_os = "macos")]
const INSTALLED_CANDIDATES: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const INSTALLED_CANDIDATES: &[&str] = &[
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/usr/bin/microsoft-edge",
];

fn is_chromium_family(exe: &Path) -> bool {
    let name = exe
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    [
        "chrome", "msedge", "brave", "vivaldi", "opera", "chromium", "thorium",
    ]
    .iter()
    .any(|b| name.contains(b))
}

fn launch_app_mode(exe: &Path, url: &str) -> bool {
    std::process::Command::new(exe)
        .arg(format!("--app={url}"))
        .arg("--window-size=1180,900")
        .spawn()
        .is_ok()
}

/// The browser Windows is configured to open http links with.
///
/// Read from the UserChoice association, then resolved to an executable via
/// the ProgId's shell command — the same path Explorer takes.
#[cfg(target_os = "windows")]
fn default_browser_exe() -> Option<std::path::PathBuf> {
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};
    use winreg::RegKey;

    let prog_id: String = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(
            r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice",
        )
        .ok()?
        .get_value("ProgId")
        .ok()?;

    let command: String = RegKey::predef(HKEY_CLASSES_ROOT)
        .open_subkey(format!(r"{prog_id}\shell\open\command"))
        .ok()?
        .get_value("")
        .ok()?;

    exe_from_command(&command)
}

#[cfg(not(target_os = "windows"))]
fn default_browser_exe() -> Option<std::path::PathBuf> {
    None
}

/// Pull the executable out of a registry shell command such as
/// `"C:\...\chrome.exe" -- "%1"`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn exe_from_command(command: &str) -> Option<std::path::PathBuf> {
    let c = command.trim();
    let path = if let Some(rest) = c.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        // Unquoted: take everything up to the first argument switch.
        c.split(" -").next()?.trim()
    };
    if path.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(path))
    }
}

#[cfg(test)]
mod tests {
    use super::{exe_from_command, sync_interval_hours};

    #[test]
    fn sync_interval_reads_prefs_and_falls_back_safely() {
        let db = crate::db::Db::open_in_memory().unwrap();
        assert_eq!(sync_interval_hours(&db), 6, "no prefs stored yet");

        db.set_setting("ui.prefs", r#"{"sync_hours":1}"#).unwrap();
        assert_eq!(sync_interval_hours(&db), 1);

        db.set_setting("ui.prefs", r#"{"sync_hours":0}"#).unwrap();
        assert_eq!(sync_interval_hours(&db), 0, "0 means never");

        db.set_setting("ui.prefs", r#"{"range":7}"#).unwrap();
        assert_eq!(sync_interval_hours(&db), 6, "absent key falls back");

        db.set_setting("ui.prefs", "not json at all").unwrap();
        assert_eq!(sync_interval_hours(&db), 6, "garbage must not disable syncing");

        db.set_setting("ui.prefs", r#"{"sync_hours":99999}"#).unwrap();
        assert_eq!(sync_interval_hours(&db), 168, "clamped to a week");
    }

    #[test]
    fn parses_quoted_and_bare_registry_commands() {
        assert_eq!(
            exe_from_command(r#""C:\Program Files\Google\Chrome\Application\chrome.exe" -- "%1""#)
                .unwrap()
                .to_str()
                .unwrap(),
            r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        );
        assert_eq!(
            exe_from_command(r"C:\firefox\firefox.exe -osint -url %1")
                .unwrap()
                .to_str()
                .unwrap(),
            r"C:\firefox\firefox.exe"
        );
        assert!(exe_from_command("").is_none());
    }
}
