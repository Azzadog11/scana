//! Linked text: every word Scana can explain is underlined in orange. Resting the pointer on it
//! shows what it means; clicking opens it in the Learn panel (with Back and Forward there).

use crate::app::ScanaApp;
use egui::{Color32, RichText, Sense, Ui};
use scana_db::library::{self, Link};

/// How a linked text looks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Look {
    pub size: Option<f32>,
    pub color: Option<Color32>,
    pub strong: bool,
}

impl Look {
    pub fn dim(pal: &crate::theme::Palette) -> Self {
        Look { color: Some(pal.dim), ..Look::default() }
    }

    pub fn strong() -> Self {
        Look { strong: true, ..Look::default() }
    }

    fn rich(self, s: &str) -> RichText {
        let mut r = RichText::new(s);
        if let Some(size) = self.size {
            r = r.size(size);
        }
        if let Some(c) = self.color {
            r = r.color(c);
        }
        if self.strong {
            r = r.strong();
        }
        r
    }
}

/// Splits text into words, each with the spaces after it, so lines wrap between words.
fn words(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_space = false;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            in_space = true;
        } else if in_space {
            out.push(&s[start..i]);
            start = i;
            in_space = false;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

impl ScanaApp {
    /// A paragraph with its known words linked.
    pub fn linked(&mut self, ui: &mut Ui, text: &str) {
        self.linked_look(ui, text, Look::default(), &[]);
    }

    /// A paragraph with its known words linked, in a given look. Links in `skip` stay plain
    /// (the topic being read doesn't link to itself).
    pub fn linked_look(&mut self, ui: &mut Ui, text: &str, look: Look, skip: &[Link]) -> Vec<Link> {
        let pal = self.pal();
        let links = library::find_links(text, skip);
        let mut clicked: Option<Link> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let plain = |ui: &mut Ui, s: &str| {
                for w in words(s) {
                    ui.label(look.rich(w));
                }
            };
            let mut pos = 0;
            for (range, link) in &links {
                plain(ui, &text[pos..range.start]);
                let word = look.rich(&text[range.clone()]).color(pal.link).underline();
                let resp = ui
                    .add(egui::Label::new(word).sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_ui(|ui| {
                        ui.set_max_width(320.0);
                        ui.label(RichText::new(link.title()).strong().color(pal.accent));
                        if let Some(s) = link.summary() {
                            ui.label(s);
                        }
                        ui.label(RichText::new("Click to read more").small().color(pal.dim));
                    });
                if resp.clicked() {
                    clicked = Some(link.clone());
                }
                pos = range.end;
            }
            plain(ui, &text[pos..]);
        });
        if let Some(link) = clicked {
            self.follow(link);
        }
        links.into_iter().map(|(_, l)| l).collect()
    }

    /// A bullet list with linked words. Returns the links drawn.
    pub fn linked_bullets(&mut self, ui: &mut Ui, items: &[String], skip: &[Link]) -> Vec<Link> {
        let mut seen: Vec<Link> = skip.to_vec();
        for item in items {
            ui.horizontal_top(|ui| {
                ui.label("•");
                let found = ui.vertical(|ui| self.linked_look(ui, item, Look::default(), &seen)).inner;
                seen.extend(found);
            });
        }
        seen
    }

    /// Small rounded buttons that open links: related topics, "read more".
    pub fn link_chips(&mut self, ui: &mut Ui, links: &[Link]) {
        let pal = self.pal();
        let mut clicked = None;
        ui.horizontal_wrapped(|ui| {
            for link in links {
                let b = egui::Button::new(RichText::new(link.title()).small().color(pal.link))
                    .corner_radius(10)
                    .stroke(egui::Stroke::new(1.0, pal.tile_border))
                    .fill(pal.tile);
                let resp = ui.add(b).on_hover_cursor(egui::CursorIcon::PointingHand);
                let resp = match link.summary() {
                    Some(s) => resp.on_hover_text(s),
                    None => resp,
                };
                if resp.clicked() {
                    clicked = Some(link.clone());
                }
            }
        });
        if let Some(link) = clicked {
            self.follow(link);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_keep_their_spaces() {
        assert_eq!(words("The engine  runs."), vec!["The ", "engine  ", "runs."]);
        assert_eq!(words(" lead"), vec![" ", "lead"]);
        assert!(words("").is_empty());
    }
}
