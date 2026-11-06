#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(any(windows, feature = "ui-check"))]
mod ui;
#[cfg(any(windows, feature = "ui-check"))]
mod ui_bots;
#[cfg(any(windows, feature = "ui-check"))]
mod ui_browser;
#[cfg(any(windows, feature = "ui-check"))]
mod ui_connectors;
#[cfg(any(windows, feature = "ui-check"))]
mod ui_dock;
#[cfg(any(windows, feature = "ui-check"))]
mod ui_settings;
#[cfg(any(windows, feature = "ui-check"))]
mod ui_vault;

fn main() {
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V")
    {
        use std::io::Write;
        let _ = writeln!(
            std::io::stdout(),
            "serabot-desktop {}",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    #[cfg(windows)]
    ui::run();
    #[cfg(not(windows))]
    eprintln!("serabot-desktop is a Windows desktop application.");
}
