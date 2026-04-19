#![windows_subsystem = "windows"]

mod app;
mod config;
mod hdr;
mod manager;
mod wmi_monitor;

/// Position used to "hide" the window while it lives in the tray.
/// Keeping it always-visible (off-screen) is more reliable than
/// Visible(false) on eframe 0.28 — the surface and event pump stay healthy.
pub const OFFSCREEN_X: f32 = -32000.0;
pub const OFFSCREEN_Y: f32 = -32000.0;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HDRify")
            .with_inner_size([520.0, 440.0])
            .with_min_inner_size([400.0, 300.0])
            .with_position([OFFSCREEN_X, OFFSCREEN_Y])
            .with_minimize_button(false)
            .with_taskbar(false),
        ..Default::default()
    };

    eframe::run_native(
        "HDRify",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
