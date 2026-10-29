#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::sync::Arc;

mod app;

fn main() -> eframe::Result<()> {
    // Console-launched smoke check (CI): print before eframe starts.
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
        return Ok(());
    }
    eframe::run_native(
        "SeraBot",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_title("SeraBot Owner")
                .with_app_id("serabot-owner")
                .with_icon(Arc::new(app_icon()))
                .with_inner_size([1120.0, 720.0])
                .with_min_inner_size([860.0, 560.0]),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(app::Desktop::new(cc)))),
    )
}

/// Generated 64 px window icon: a teal disc with a dark chat glyph, so the
/// build ships no image asset. Rasterized at compile time.
fn app_icon() -> eframe::egui::IconData {
    const SIZE: usize = 64;
    let teal = [92u8, 200, 182, 255];
    let dark = [14u8, 17, 22, 255];
    let panel = [20u8, 26, 33, 255];
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let fx = x as f32;
            let fy = y as f32;
            let pixel = if in_rounded_rect(fx, fy, 18.0, 21.0, 46.0, 39.0, 6.0) || in_tail(fx, fy) {
                panel
            } else {
                let dx = fx - 31.5;
                let dy = fy - 31.5;
                if dx * dx + dy * dy <= 23.0 * 23.0 {
                    teal
                } else if in_rounded_rect(fx, fy, 1.0, 1.0, 63.0, 63.0, 14.0) {
                    dark
                } else {
                    [0, 0, 0, 0]
                }
            };
            let index = (y * SIZE + x) * 4;
            rgba[index..index + 4].copy_from_slice(&pixel);
        }
    }
    eframe::egui::IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

fn in_rounded_rect(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> bool {
    if x < left || x > right || y < top || y > bottom {
        return false;
    }
    let cx = x.clamp(left + radius, right - radius);
    let cy = y.clamp(top + radius, bottom - radius);
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy <= radius * radius
}

fn in_tail(x: f32, y: f32) -> bool {
    (39.0..=47.0).contains(&y) && x >= 24.0 && x <= 24.0 + (47.0 - y) * 1.3
}
