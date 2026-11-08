#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod camera;
mod chart;
mod overlay;
mod record;
mod sim;
mod stream;
mod thermal;
mod usbcam;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut screenshot: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--screenshot" {
            screenshot = args.get(i + 1).map(PathBuf::from);
            i += 1;
        } else if let Some(p) = args[i].strip_prefix("--screenshot=") {
            screenshot = Some(PathBuf::from(p));
        }
        i += 1;
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Wärmebild-Studio")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1040.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Wärmebild-Studio",
        options,
        Box::new(move |cc| Ok(Box::new(app::ThermoApp::new(cc, screenshot)))),
    )
}
