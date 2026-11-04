//! Headway — a fast, friendly git client with workspaces.

// Release builds on Windows open no console window alongside the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod git;
mod graph;
mod overview;
mod resolver;
mod samples;
mod theme;
mod views;
mod workspace;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let mut screenshot = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => screenshot = args.next().map(PathBuf::from),
            // Development builds only: release builds ship without the sample projects.
            #[cfg(debug_assertions)]
            "--reset-sample" => {
                // Rebuild one of the sample projects from scratch, then exit.
                let name = args.next().unwrap_or_default();
                let path = dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("headway").join("samples").join(&name);
                match samples::reset_sample(&path) {
                    Ok(()) => println!("Reset the sample project {name}"),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "--help" | "-h" => {
                if cfg!(debug_assertions) {
                    println!("headway [--screenshot <file.png>] [--reset-sample web-shop|api-server|design-tokens]");
                } else {
                    println!("headway [--screenshot <file.png>]");
                }
                return Ok(());
            }
            _ => {}
        }
    }

    // The taskbar/title-bar icon. On Wayland the desktop entry (assets/linux) supplies it instead.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon/headway-256.png")).expect("bundled icon is a valid PNG");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Headway")
            .with_app_id("headway")
            .with_icon(icon)
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("Headway", options, Box::new(move |cc| Ok(Box::new(app::HeadwayApp::new(cc, screenshot)))))
}
