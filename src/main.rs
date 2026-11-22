#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

use std::sync::Arc;

use polydesk::CoreEngine;
use polydesk::updater::cleanup_previous_update;
use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
    cleanup_previous_update();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("polydesk=info,warn")),
        )
        .with_target(false)
        .init();

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed creating Tokio runtime"),
    );

    let core = {
        let _guard = rt.enter();
        CoreEngine::start(None).expect("failed starting PolyDesk core engine")
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PolyDesk — Low-Latency Polymarket CLOB V2 Terminal")
            .with_inner_size([1480.0, 900.0])
            .with_min_inner_size([1024.0, 680.0]),
        ..Default::default()
    };

    eframe::run_native(
        "PolyDesk",
        options,
        Box::new(move |cc| Ok(Box::new(ui::PolyDeskDesktop::new(cc, core, rt)))),
    )
}
