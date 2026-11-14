//! First run of the desktop app (Type and Writer): ask for the name and initials that comments
//! and tracked changes carry, as Office does, instead of a stand-in name.

use crate::WordApp;

fn ids() -> (egui::Id, egui::Id, egui::Id) {
    (egui::Id::unique("first-run-name"), egui::Id::unique("first-run-initials"), egui::Id::unique("first-run-later"))
}

pub fn show(app: &mut WordApp, ctx: &egui::Context) {
    if !app.services.ask_name || app.session.author != wordcraft_engine::DEFAULT_AUTHOR {
        return;
    }
    let (name_id, initials_id, later_id) = ids();
    if ctx.data(|d| d.get_temp::<bool>(later_id)).unwrap_or(false) {
        return;
    }
    let mut name: String = ctx.data(|d| d.get_temp(name_id)).unwrap_or_default();
    let mut initials: String = ctx.data(|d| d.get_temp(initials_id)).unwrap_or_default();
    let (mut ok, mut later) = (false, false);
    egui::Window::new(tl!("Your name"))
        .id(egui::Id::unique("first-run-window"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
        .show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.label(tl!("Comments and tracked changes carry this name and these initials."));
            ui.add_space(6.0);
            egui::Grid::new("first-run-grid").num_columns(2).spacing(egui::vec2(8.0, 6.0)).show(ui, |ui| {
                ui.label(tl!("User name:"));
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(240.0).hint_text("Alex Reyes"));
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) && r.lost_focus() {
                    ok = !name.trim().is_empty();
                }
                ui.end_row();
                ui.label(tl!("Initials:"));
                let auto: String = name.split_whitespace().filter_map(|w| w.chars().next()).collect();
                ui.add(egui::TextEdit::singleline(&mut initials).desired_width(80.0).hint_text(auto));
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!name.trim().is_empty(), egui::Button::new(tl!("OK"))).clicked() {
                    ok = true;
                }
                if ui.button(tl!("Later")).clicked() {
                    later = true;
                }
            });
        });
    if ok {
        app.session.author = name.trim().to_string();
        app.session.initials = initials.trim().to_string();
        let _ = app.save_prefs_now();
    } else if later {
        ctx.data_mut(|d| d.insert_temp(later_id, true));
    } else {
        ctx.data_mut(|d| {
            d.insert_temp(name_id, name);
            d.insert_temp(initials_id, initials);
        });
    }
}
