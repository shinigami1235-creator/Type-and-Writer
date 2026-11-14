//! Home › Dictate (Type and Writer): while it listens, a small panel says so and what is said
//! lands at the caret a phrase at a time. The listening itself is the desktop app's
//! ([`crate::Dictation`]).

use serde_json::json;

use crate::WordApp;

fn error_id() -> egui::Id {
    egui::Id::unique("dictate-error")
}

/// Show `msg` in a small window until OK is pressed.
pub fn notify(ctx: Option<&egui::Context>, msg: String) {
    set_error(ctx, msg);
}

fn set_error(ctx: Option<&egui::Context>, msg: String) {
    if let Some(ctx) = ctx {
        ctx.data_mut(|d| d.insert_temp(error_id(), Some(msg)));
    }
}

/// `what`: "start", "stop" or "toggle".
pub fn request(app: &mut WordApp, what: &str) {
    let ctx = app.ctx.clone();
    let Some(d) = app.services.dictation.as_mut() else {
        set_error(ctx.as_ref(), tl!("Dictation isn't available in this version of Type and Writer.").to_string());
        return;
    };
    let start = match what {
        "start" => true,
        "stop" => false,
        _ => !d.listening(),
    };
    if start {
        if let Err(e) = d.start() {
            set_error(ctx.as_ref(), e);
        }
    } else {
        d.stop();
    }
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}

/// Whether Dictate is on (for the ribbon button).
pub fn listening(app: &WordApp) -> bool {
    app.services.dictation.as_ref().is_some_and(|d| d.listening())
}

/// Type what was heard, and show the listening panel or a problem.
pub fn show(app: &mut WordApp, ctx: &egui::Context) {
    let mut heard = Vec::new();
    let mut problem = None;
    let on = match app.services.dictation.as_mut() {
        Some(d) => {
            heard = d.take_text();
            problem = d.take_error();
            d.listening()
        }
        None => false,
    };
    if let Some(p) = problem {
        set_error(Some(ctx), p);
    }
    for phrase in heard {
        let phrase = phrase.trim();
        if phrase.is_empty() {
            continue;
        }
        // A phrase after text on the line gets a space before it.
        let at_start = app.session.sel.focus.off == 0;
        let text = if at_start { phrase.to_string() } else { format!(" {phrase}") };
        let _ = app.run("text.insert", json!({"text": text}));
    }
    if on {
        let mut stop = false;
        egui::Window::new(tl!("Dictate"))
            .id(egui::Id::unique("dictate-panel"))
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -48.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let t = ui.input(|i| i.time);
                    let pulse = 0.55 + 0.45 * ((t * 3.0).sin() as f32).abs();
                    let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 5.0 * pulse + 1.0, egui::Color32::from_rgb(214, 48, 49));
                    ui.label(tl!("Listening. Words appear at the cursor after each pause."));
                    if ui.button(tl!("Stop")).clicked() {
                        stop = true;
                    }
                });
            });
        if stop && let Some(d) = app.services.dictation.as_mut() {
            d.stop();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
    }
    let err: Option<String> = ctx.data(|d| d.get_temp::<Option<String>>(error_id())).flatten();
    if let Some(msg) = err {
        let mut close = false;
        egui::Window::new("Type and Writer")
            .id(egui::Id::unique("dictate-error-window"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(msg);
                ui.add_space(6.0);
                if ui.button(tl!("OK")).clicked() {
                    close = true;
                }
            });
        if close {
            ctx.data_mut(|d| d.insert_temp::<Option<String>>(error_id(), None));
        }
    }
}
