# Native shell

A [Tauri](https://tauri.app) window around the same server the CLI runs. It is
optional — `t-fit` on its own already opens a chromeless Edge/Chrome window,
which looks and behaves much the same.

This crate is deliberately **not** in the root workspace, so `cargo build` at
the repo root works on a machine with no WebView toolchain.

```
cargo install tauri-cli --version "^2"
cd desktop
cargo tauri build
```

Requirements: WebView2 on Windows (already installed on Windows 10/11),
`webkit2gtk-4.1` on Linux, nothing extra on macOS.

`icons/` needs `icon.png` (512×512) and `icon.ico` before a release build.
`cargo tauri icon path/to/logo.png` generates the whole set.
