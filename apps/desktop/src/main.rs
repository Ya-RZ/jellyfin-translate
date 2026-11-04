//! Jellyfin Translate desktop app (Windows).
//!
//! Entry point: builds the tokio runtime used for background work, applies the
//! UI/UX v2 theme, then opens the GPUI window with [`app::AppState`] as its
//! root view.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod app;
mod ui;
mod views;

use app::AppState;
use gpui_kit::*;

fn main() {
    // All network and SQLite work is spawned onto this runtime. The UI thread
    // never awaits it directly: background tasks report back over an mpsc
    // channel consumed by GPUI foreground tasks.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to build the tokio runtime");
    let runtime_handle = runtime.handle().clone();

    let application = gpui_kit::application().with_assets(gpui_kit::assets::Assets);
    application.run(move |cx| {
        gpui_kit::init(cx);
        ui::apply_theme(cx);
        gpui_kit::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Jellyfin Translate".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1280.), px(820.)),
                    cx,
                ))),
                ..Default::default()
            },
            cx,
            move |window, cx| cx.new(|cx| AppState::new(runtime_handle.clone(), window, cx)),
        )
        .expect("failed to open the main window");
    });

    // Don't wait for in-flight jobs when the window closes.
    runtime.shutdown_background();
}
