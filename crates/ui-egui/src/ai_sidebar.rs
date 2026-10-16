//! Native assistant panels, shown beside the existing right-hand tool rail.

use crate::{PhotocraftApp, icons, theme};
use egui::{Rect, Response, Sense, Stroke, Vec2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tab {
    #[default]
    Codex,
    YorusAi,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub tab: Tab,
}

pub fn available(app: &PhotocraftApp) -> bool {
    app.services.codex_chat.is_some() || app.services.generate_image.is_some() || app.services.yorus_ai_generate.is_some()
}

pub fn open(app: &mut PhotocraftApp, tab: Tab) {
    app.ui.codex.open = true;
    app.ui.ai_sidebar.tab = tab;
}

pub fn toggle(app: &mut PhotocraftApp, tab: Tab) {
    if app.ui.codex.open && app.ui.ai_sidebar.tab == tab {
        app.ui.codex.open = false;
    } else {
        open(app, tab);
    }
}

/// A neutral palette that follows the app's light/dark appearance.
pub fn palette(ui: &egui::Ui) -> theme::Tokens {
    theme::Tokens::for_kind(if ui.visuals().dark_mode { theme::ThemeKind::Pro } else { theme::ThemeKind::ProLight })
}

pub fn gpt_image(dark: bool, size: f32) -> egui::Image<'static> {
    let source = if dark {
        egui::include_image!("../../../assets/yorusphoto/OAI_OpenAI-Blossom_White.svg")
    } else {
        egui::include_image!("../../../assets/yorusphoto/OAI_OpenAI-Blossom_Black.svg")
    };
    egui::Image::new(source).fit_to_exact_size(Vec2::splat(size))
}

pub fn gpt_mark(ui: &egui::Ui, rect: Rect) {
    gpt_image(ui.visuals().dark_mode, rect.width()).paint_at(ui, rect);
}

pub fn rail_button(ui: &mut egui::Ui, size: f32, active: bool) -> Response {
    let t = theme::Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if active || response.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, if active { t.row_selected } else { t.hover });
    }
    gpt_mark(ui, Rect::from_center_size(rect.center(), Vec2::splat(size * 1.35)));
    response.on_hover_text("Codex · ChatGPT")
}

pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if !available(app) || !app.ui.codex.open {
        return;
    }
    let t = palette(ui);
    crate::direction::end_panel("yorus-assistant-sidebar")
        .resizable(true)
        .default_size(370.0)
        .size_range(300.0..=560.0)
        .frame(egui::Frame::NONE.fill(t.canvas).inner_margin(egui::Margin::same(16)).stroke(Stroke::new(1.0, t.separator)))
        .show(ui, |ui| {
            ui.visuals_mut().override_text_color = Some(t.text);
            ui.visuals_mut().widgets.inactive.bg_fill = t.field;
            ui.visuals_mut().widgets.inactive.weak_bg_fill = t.field;
            ui.visuals_mut().widgets.inactive.fg_stroke.color = t.text;
            ui.visuals_mut().widgets.hovered.bg_fill = t.hover;
            ui.visuals_mut().widgets.hovered.fg_stroke.color = t.text;
            ui.visuals_mut().widgets.active.bg_fill = t.pressed;
            ui.visuals_mut().widgets.active.fg_stroke.color = t.text;
            ui.visuals_mut().extreme_bg_color = t.field;
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 10.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                if app.ui.ai_sidebar.tab == Tab::Codex {
                    gpt_mark(ui, rect.expand(5.0));
                } else {
                    crate::brand::paint_logo(ui, rect);
                }
                ui.selectable_value(&mut app.ui.ai_sidebar.tab, Tab::Codex, egui::RichText::new("Codex").strong());
                ui.selectable_value(&mut app.ui.ai_sidebar.tab, Tab::YorusAi, "YorusAi");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, "x", 24.0, false, "Close assistant panel").clicked() {
                        app.ui.codex.open = false;
                    }
                });
            });
            ui.add_space(6.0);
            match app.ui.ai_sidebar.tab {
                Tab::Codex => crate::codex_ui::panel(app, ui),
                Tab::YorusAi => crate::yorus_ai_ui::panel(app, ui),
            }
        });
}

/// Neutral rounded action, shared by assistant composers.
pub fn send_button(ui: &mut egui::Ui, icon: &str, enabled: bool, label: &str) -> Response {
    let t = palette(ui);
    ui.add_enabled(
        enabled,
        egui::Button::image(icons::image(icon, 17.0, t.canvas))
            .fill(if enabled { t.text } else { t.text_faint })
            .corner_radius(20.0)
            .min_size(Vec2::splat(34.0)),
    )
    .on_hover_text(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_rail_toggles_the_active_panel_and_switches_tabs() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        open(&mut app, Tab::Codex);
        toggle(&mut app, Tab::Codex);
        assert!(!app.ui.codex.open);
        toggle(&mut app, Tab::YorusAi);
        assert!(app.ui.codex.open);
        assert_eq!(app.ui.ai_sidebar.tab, Tab::YorusAi);
        toggle(&mut app, Tab::Codex);
        assert!(app.ui.codex.open);
        assert_eq!(app.ui.ai_sidebar.tab, Tab::Codex);
    }
}
