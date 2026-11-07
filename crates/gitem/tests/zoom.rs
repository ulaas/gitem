//! Interface zoom, from Settings and from egui's own Ctrl+Plus.
gitem_testenv::isolate!();

use egui::vec2;
use egui_kittest::Harness;
use gitem::app::GitemApp;
use gitem_core::settings::Settings;

fn app() -> Harness<'static, GitemApp> {
    let ctx = egui::Context::default();
    let app = GitemApp::with_settings(&ctx, Settings::default());
    Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5).build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app)
}

/// Zoom set takes a frame to apply. Reading egui's back before then undid the change, and the
/// two values then swapped every frame, drawing the UI at both sizes on top of each other.
#[test]
fn zoom_chosen_in_settings_stays_put() {
    let mut h = app();
    for _ in 0..3 {
        h.step();
    }
    h.state_mut().settings.ui_scale = 1.5;
    let mut seen = Vec::new();
    for _ in 0..8 {
        h.step();
        seen.push((h.ctx.zoom_factor(), h.state().settings.ui_scale));
    }
    assert!(seen[2..].iter().all(|&(z, s)| (z - 1.5).abs() < 0.001 && (s - 1.5).abs() < 0.001), "{seen:?}");
}

#[test]
fn zoom_changed_by_egui_is_kept_in_settings() {
    let mut h = app();
    for _ in 0..3 {
        h.step();
    }
    // What Ctrl+Plus does.
    h.ctx.set_zoom_factor(1.25);
    for _ in 0..5 {
        h.step();
    }
    assert!((h.ctx.zoom_factor() - 1.25).abs() < 0.001);
    assert!((h.state().settings.ui_scale - 1.25).abs() < 0.001);
}
