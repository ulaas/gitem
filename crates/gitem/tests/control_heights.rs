//! Controls side by side stand the same height: buttons, small buttons, menus, text boxes and
//! icon buttons. Each used to work out its own, so a row of them looked ragged.
gitem_testenv::isolate!();

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use gitem::theme::{self, Palette};
use gitem_core::settings::ThemeChoice;

#[test]
fn every_kind_of_control_in_a_row_is_as_tall_as_the_others() {
    let pal = Palette::get(ThemeChoice::Dark);
    let mut text = String::new();
    let mut pick = 0;
    let mut checked = false;
    let mut h = Harness::builder().with_size(vec2(900.0, 120.0)).build_ui(move |ui| {
        theme::apply(ui.ctx(), &pal, 1.0, false);
        ui.horizontal(|ui| {
            let _ = ui.button("Button");
            let _ = gitem::widgets::small_button(ui, "Small");
            ui.add(gitem::widgets::input(&mut text).hint_text("Box").desired_width(80.0));
            egui::ComboBox::from_id_salt("menu").selected_text("Menu").show_ui(ui, |ui| {
                ui.selectable_value(&mut pick, 0, "a");
            });
            let _ = ui.menu_button("Menu button", |_| {});
            let _ = gitem::chrome::icon_button(ui, gitem::chrome::icon::GEAR_SIX, "Icon", "", false, &pal);
            let _ = gitem::chrome::tool_button(ui, gitem::chrome::icon::ARCHIVE, pal.text, "Tool", "", false, &pal);
            let _ = gitem::chrome::icon_text_button(ui, gitem::chrome::icon::ARROW_DOWN, pal.text, "Pull", "", false, &pal);
            ui.checkbox(&mut checked, "Tick");
        });
    });
    h.run();
    let want = theme::CONTROL_HEIGHT;
    for name in ["Button", "Small", "Menu button", "Icon", "Tool", "Pull"] {
        let r = h.get_by_label(name).rect();
        assert!((r.height() - want).abs() < 0.6, "{name} is {} tall, not {want}", r.height());
    }
    let boxed = h.get_by_role(egui::accesskit::Role::TextInput).rect();
    assert!((boxed.height() - want).abs() < 0.6, "the text box is {} tall", boxed.height());
}
