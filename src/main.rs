//! Headway — a fast, friendly git client with workspaces.

// Release builds on Windows open no console window alongside the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cmdlog;
mod diffmodel;
mod fileview;
mod forge;
mod git;
mod graph;
mod highlight;
mod markdown;
mod newpr;
mod oplog;
mod overview;
mod preview;
mod prview;
mod resolver;
mod samples;
mod theme;
mod views;
mod workspace;

use std::path::PathBuf;

fn main() -> eframe::Result {
    // Only development builds can set this (--screenshot); release builds always start normally.
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut screenshot: Option<PathBuf> = None;
    let mut export: Option<PathBuf> = None;
    let mut days = 2;
    let all: Vec<String> = std::env::args().skip(1).collect();
    if all.iter().any(|a| matches!(a.as_str(), "--help" | "-h" | "--version" | "-V" | "--log-path" | "--export-log")) {
        attach_console();
    }
    let mut args = all.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" | "-V" => {
                println!("headway {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            // Development builds only: save a picture of the window once it has loaded, then quit.
            #[cfg(debug_assertions)]
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
                println!("headway                                  open Headway");
                println!("headway --version, -V                    print the version");
                println!("headway --help, -h                       show this help");
                println!("headway --log-path                       print the command log folder");
                println!("headway --export-log <file> [--days N]   save the last N days (default 2) of the command log to attach to a bug report");
                if cfg!(debug_assertions) {
                    println!();
                    println!("Development builds only:");
                    println!("headway --screenshot <file.png>          save a picture of the window once it has loaded, then quit");
                    println!("headway --reset-sample <name>            rebuild a sample project: web-shop|api-server|design-tokens|price-rules|diff-showcase");
                    println!("HEADWAY_FAKE_FORGE=<folder>              answer GitHub and GitLab questions from <folder>/forge.json instead of gh and glab");
                }
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
