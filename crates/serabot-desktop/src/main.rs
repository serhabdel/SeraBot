#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(any(windows, feature = "ui-check"))]
mod ui;

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
