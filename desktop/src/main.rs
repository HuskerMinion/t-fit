//! Native window around the t-fit server.
//!
//! The server is the same one `t-fit --serve` runs; this just starts it on
//! loopback and points a WebView at it. Nothing about the app lives here, so
//! the two front doors can never drift apart.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// The port the Withings callback URL is registered against. The desktop app
/// has to prefer it, or OAuth breaks: Withings compares the redirect_uri
/// against the one in your developer app, character for character, and a
/// randomly chosen port would never match.
const PREFERRED_PORT: u16 = 8787;

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    // Take 8787 when it's free. If something else already holds it — most
    // likely a `t-fit --serve` in a terminal — fall back to any free port so
    // the window still opens; Withings sync just won't link from this
    // instance until the other one is closed.
    let (listener, addr) = rt
        .block_on(async {
            match t_fit::bind(format!("127.0.0.1:{PREFERRED_PORT}").parse().unwrap()).await {
                Ok(v) => Ok(v),
                Err(_) => t_fit::bind("127.0.0.1:0".parse().unwrap()).await,
            }
        })
        .expect("could not bind a local port");
    if addr.port() != PREFERRED_PORT {
        eprintln!(
            "port {PREFERRED_PORT} was busy; using {} instead. Withings linking needs {PREFERRED_PORT}.",
            addr.port()
        );
    }
    let url = format!("http://127.0.0.1:{}", addr.port());

    let db_path = t_fit::default_db_path();
    let serve_url = url.clone();
    std::thread::spawn(move || {
        rt.block_on(async {
            if let Err(e) = t_fit::serve(listener, &db_path, serve_url).await {
                eprintln!("t-fit server stopped: {e:#}");
            }
        });
    });

    tauri::Builder::default()
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title("t-fit")
                .inner_size(1180.0, 900.0)
                .min_inner_size(360.0, 520.0)
                .build()?;
            let _ = app.handle();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start t-fit");
}
