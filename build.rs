//! Stamps the build time into the binary, so the running app can say which
//! build it is. Without this, an installed copy and a fresh `cargo build`
//! look identical from the outside.

fn main() {
    println!(
        "cargo:rustc-env=BUILD_DATE={}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    );
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=web");
    println!("cargo:rerun-if-changed=Cargo.toml");
}
