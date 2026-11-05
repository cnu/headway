//! Headway — a fast, friendly git client with workspaces.

// Release builds on Windows open no console window alongside the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cmdlog;
mod diffmodel;
mod fileview;
mod git;
mod graph;
mod highlight;
mod oplog;
mod overview;
mod preview;
mod resolver;
mod samples;
mod theme;
mod views;
mod workspace;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let mut screenshot = None;
    let mut export: Option<PathBuf> = None;
    let mut days = 2;
    let all: Vec<String> = std::env::args().skip(1).collect();
    if all.iter().any(|a| matches!(a.as_str(), "--help" | "-h" | "--log-path" | "--export-log")) {
        attach_console();
    }
    let mut args = all.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => screenshot = args.next().map(PathBuf::from),
            "--log-path" => {
                println!("{}", cmdlog::log_path().display());
                return Ok(());
            }
            "--export-log" => match args.next() {
                Some(f) => export = Some(PathBuf::from(f)),
                None => fail("--export-log needs a file to write"),
            },
            "--days" => match args.next().and_then(|d| d.parse().ok()) {
                Some(d) if d > 0 => days = d,
                _ => fail("--days needs a number of days"),
            },
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
                    println!("headway [--screenshot <file.png>] [--reset-sample web-shop|api-server|design-tokens|price-rules|diff-showcase]");
                } else {
                    println!("headway [--screenshot <file.png>]");
                }
                println!("headway --log-path                       print the command log folder");
                println!("headway --export-log <file> [--days N]   save the last N days (default 2) of the command log to attach to a bug report");
                println!();
                println!("HEADWAY_LOG=normal|verbose|off sets how much is logged; HEADWAY_LOG_DIR=<folder> moves the log.");
                return Ok(());
            }
            _ => {}
        }
    }
    if let Some(file) = export {
        match cmdlog::export(&file, days) {
            Ok(n) => println!("Saved {n} log entries to {}", file.display()),
            Err(e) => fail(&format!("Couldn't export the command log from {}: {e}", cmdlog::log_path().display())),
        }
        return Ok(());
    }

    // The command log, for the real app only (not for screenshots).
    if screenshot.is_none() {
        cmdlog::init(cmdlog::Config::from_env());
        cmdlog::install_panic_hook();
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
    let result = eframe::run_native("Headway", options, Box::new(move |cc| Ok(Box::new(app::HeadwayApp::new(cc, screenshot)))));
    cmdlog::shutdown();
    result
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

/// Release builds on Windows have no console of their own, so command-line
/// output goes nowhere unless we borrow the one Headway was started from.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: no pointers; fails harmlessly when there's no parent console or one is attached already.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}
