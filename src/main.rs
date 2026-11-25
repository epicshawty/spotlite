// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use spotlite::app::Spotlite;
use spotlite::config::Config;
use spotlite::soloist;

fn main() -> eframe::Result<()> {
    if std::env::args().any(|a| a == "--check" || a == "-V" || a == "--version") {
        check();
        return Ok(());
    }

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([960.0, 600.0])
            .with_title("Spotlite"),
        ..Default::default()
    };
    eframe::run_native(
        "Spotlite",
        options,
        Box::new(|cc| Ok(Box::new(Spotlite::new(cc)))),
    )
}

/// `spotlite --check` prints what CI and a support thread need, without a window.
fn check() {
    let cfg = Config::load();
    println!("spotlite {}", env!("CARGO_PKG_VERSION"));
    println!("config   {}", Config::path().display());
    println!(
        "signed   {}",
        if cfg.refresh_token.is_empty() {
            "no"
        } else {
            "yes"
        }
    );
    println!(
        "client   {}",
        if cfg.client_id.is_empty() {
            "unset"
        } else {
            "set"
        }
    );
    let path = soloist::default_path();
    println!(
        "soloist  {} ({})",
        path.display(),
        if path.exists() { "found" } else { "missing" }
    );
}
