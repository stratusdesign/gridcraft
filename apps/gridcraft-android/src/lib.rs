//! GridCraft as an Android app (phones, tablets and ChromeOS, which runs Android apps natively).
//!
//! Runs the same [`gridcraft_ui_egui::SheetApp`] as the desktop app through eframe's Android
//! runner (winit NativeActivity, wgpu on Vulkan or GLES). Build with `cargo apk build --release
//! -p gridcraft-android` (see `.github/workflows/android.yml`).
//!
//! First cut: no system file picker yet (Android only returns picker results to a Java
//! activity). Preferences live in the app's private storage.
#![cfg(target_os = "android")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::PathBuf;

use eframe::winit::platform::android::activity::AndroidApp;
use gridcraft_engine::Session;
use gridcraft_ui_egui::doc_windows::DocWindows;
use gridcraft_ui_egui::{Services, SheetApp};

struct App(SheetApp, DocWindows, Option<PathBuf>);

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.logic(ctx);
        self.1.logic(&mut self.0, ctx);
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.0.raw_input_hook(ctx, raw);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.1.show(&mut self.0, ui);
    }
    fn on_exit(&mut self) {
        save_prefs(&self.0, self.2.as_deref());
    }
}

fn save_prefs(app: &SheetApp, dir: Option<&std::path::Path>) {
    let Some(dir) = dir else { return };
    let _ = std::fs::create_dir_all(dir);
    if let Ok(b) = serde_json::to_vec_pretty(&app.ui) {
        let _ = std::fs::write(dir.join("ui.json"), b);
    }
    if let Ok(b) = serde_json::to_vec_pretty(&app.session.prefs) {
        let _ = std::fs::write(dir.join("prefs.json"), b);
    }
}

fn load_prefs(app: &mut SheetApp, dir: Option<&std::path::Path>) {
    app.ui.system_theme = true;
    let Some(dir) = dir else { return };
    if let Ok(b) = std::fs::read(dir.join("ui.json"))
        && let Ok(ui) = serde_json::from_slice::<gridcraft_ui_egui::UiState>(&b)
    {
        app.ui = ui.at_startup();
    }
    if let Ok(b) = std::fs::read(dir.join("prefs.json"))
        && let Ok(p) = serde_json::from_slice(&b)
    {
        app.session.prefs = p;
    }
}

#[unsafe(no_mangle)]
fn android_main(android_app: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("gridcraft"));
    let prefs_dir = android_app.internal_data_path();
    let mut app = SheetApp::new(Session::new(), Services::default());
    load_prefs(&mut app, prefs_dir.as_deref());
    if app.session.documents().is_empty() {
        app.session.new_workbook();
    }
    app.after_engine();
    let viewport = egui::ViewportBuilder::default().with_title("GridCraft");
    let window_builder = viewport.clone();
    let options = eframe::NativeOptions { viewport, android_app: Some(android_app), ..Default::default() };
    let pending = std::cell::Cell::new(Some(app));
    let result = eframe::run_native(
        "GridCraft",
        options,
        Box::new(|cc| {
            let Some(mut app) = pending.take() else {
                return Err("the app was already created".into());
            };
            SheetApp::setup_context_for_language(&cc.egui_ctx, app.ui.dark, app.ui.language);
            let windows = DocWindows::new(&mut app, window_builder.clone(), true);
            Ok(Box::new(App(app, windows, prefs_dir.clone())))
        }),
    );
    if let Err(e) = result {
        log::error!("GridCraft stopped: {e}");
    }
}
