//! t-fit — a local-first weight tracker.
//!
//!   t-fit                    → opens an app window on this machine
//!   t-fit --tab              → opens an ordinary browser tab instead
//!   t-fit --serve 0.0.0.0    → headless, reachable from your phone on the LAN
//!   t-fit --import file.csv  → load a CSV and exit
//!
//! All state lives in one SQLite file you can copy, sync or back up.

use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use t_fit::{bind, db, default_db_path, fitday, import, local_ip, model, open_app_window, serve};

#[derive(Parser, Debug)]
#[command(name = "t-fit", version, about = "A modern, local-first weight tracker")]
struct Args {
    /// Address to bind. Use 0.0.0.0 to reach it from other devices.
    #[arg(long, default_value = "127.0.0.1")]
    serve: String,

    #[arg(long, default_value_t = 8787)]
    port: u16,

    /// Database file. Defaults to a per-user data directory.
    #[arg(long)]
    db: Option<PathBuf>,

    /// Open a normal browser tab rather than an app window.
    #[arg(long)]
    tab: bool,

    /// Browser to open the app window with. Defaults to your system default
    /// browser when it supports app mode.
    #[arg(long)]
    browser: Option<PathBuf>,

    /// Don't open anything — just serve.
    #[arg(long)]
    no_open: bool,

    /// Public origin for the Withings redirect URI. Defaults to
    /// http://127.0.0.1:<port>, which is what you register with Withings.
    #[arg(long)]
    base_url: Option<String>,

    /// Import a CSV and exit.
    #[arg(long)]
    import: Option<PathBuf>,

    /// Import a FitDay PC .fdy or .fbk file directly, and exit.
    #[arg(long, value_name = "FILE")]
    import_fitday: Option<PathBuf>,

    /// With --import, replace days that already exist.
    #[arg(long)]
    overwrite: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "t_fit=info".into()),
        )
        .init();

    let args = Args::parse();
    let path = args.db.clone().unwrap_or_else(default_db_path);

    if let Some(fdy) = args.import_fitday {
        let d = db::Db::open(&path)?;
        let bytes = std::fs::read(&fdy)?;
        let report = fitday::parse(&bytes)?;
        let mut added = 0usize;
        let mut existing = 0usize;
        for e in &report.entries {
            if args.overwrite {
                d.upsert(e)?;
                added += 1;
            } else if d.insert_if_absent(e)? {
                added += 1;
            } else {
                existing += 1;
            }
        }
        println!(
            "read {} days from {} ({} with notes)",
            report.entries.len(),
            fdy.display(),
            report.with_notes
        );
        if let (Some(a), Some(b)) = (report.entries.first(), report.entries.last()) {
            println!("  {} → {}", a.date, b.date);
        }
        println!("  added {added}, already present {existing}");
        if report.unresolved > 0 {
            eprintln!(
                "  warning: {} index entries could not be resolved to a record — \
                 the decode may be incomplete",
                report.unresolved
            );
        }
        return Ok(());
    }

    if let Some(csv_path) = args.import {
        let d = db::Db::open(&path)?;
        let text = std::fs::read_to_string(&csv_path)?;
        let rep = import::import_csv(&d, &text, model::Source::Import, args.overwrite)?;
        println!(
            "imported {} of {} rows ({} already present, {} errors)",
            rep.inserted,
            rep.read,
            rep.skipped_existing,
            rep.errors.len()
        );
        for e in rep.errors.iter().take(10) {
            eprintln!("  {e}");
        }
        return Ok(());
    }

    let lan = args.serve == "0.0.0.0";
    let addr: SocketAddr = format!("{}:{}", args.serve, args.port).parse()?;
    let (listener, actual) = bind(addr).await?;

    let local = format!("http://127.0.0.1:{}", actual.port());
    let shown = if lan {
        format!("http://{}:{}", local_ip().unwrap_or_else(|| "0.0.0.0".into()), actual.port())
    } else {
        local.clone()
    };
    println!("\n  t-fit is running at {shown}\n");

    if !args.no_open && !lan {
        if args.tab {
            let _ = open::that_detached(&local);
        } else {
            open_app_window(&local, args.browser.as_deref());
        }
    }

    serve(listener, &path, args.base_url.unwrap_or(local)).await
}
