use egui::{Response, TextEdit, Ui, Widget};

/// Focus-safe reusable search input widget.
///
/// Ensures the TextEdit is always laid out first, preventing dynamic "Clear" buttons
/// from stealing focus or disrupting the cursor position during continuous typing.
pub struct SearchInput<'a> {
    text: &'a mut String,
    hint: &'a str,
    id_salt: &'a str,
    desired_width: Option<f32>,
}

impl<'a> SearchInput<'a> {
    pub fn new(text: &'a mut String, hint: &'a str) -> Self {
        Self {
            text,
            hint,
            id_salt: "search_input",
            desired_width: None,
        }
    }

    pub fn id_salt(mut self, salt: &'a str) -> Self {
        self.id_salt = salt;
        self
    }

    pub fn desired_width(mut self, width: f32) -> Self {
        self.desired_width = Some(width);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let text_ref = self.text;
        let hint = self.hint;
        let id_salt = self.id_salt;
        let width = self.desired_width;

        ui.horizontal(|ui| {
            let mut edit = TextEdit::singleline(text_ref)
                .id_salt(id_salt)
                .hint_text(hint);

            if let Some(w) = width {
                edit = edit.desired_width(w);
            }

            let response = ui.add(edit);

            if !text_ref.is_empty() {
                if ui.button("✖").on_hover_text("Clear search").clicked() {
                    text_ref.clear();
                    response.request_focus();
                }
            }

            response
        })
        .inner
    }
}

impl<'a> Widget for SearchInput<'a> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui)
    }
}

/// A reusable dropdown with an embedded typeahead search filter.
pub struct SearchablePicker;

impl SearchablePicker {
    pub fn show<'a, T>(
        ui: &mut Ui,
        id_salt: impl std::hash::Hash,
        selected_text: &str,
        filter: &mut String,
        items: &'a [T],
        label_fn: impl Fn(&T) -> String,
    ) -> Option<&'a T> {
        let popup_id = ui.make_persistent_id(id_salt);
        let button_text = format!("{} ⏷", selected_text);
        let btn_resp = ui.button(button_text);
        let was_opened = btn_resp.clicked();
        if was_opened {
            ui.memory_mut(|mem| mem.toggle_popup(popup_id));
        }

        let mut selected = None;

        egui::popup_above_or_below_widget(
            ui,
            popup_id,
            &btn_resp,
            egui::AboveOrBelow::Below,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(220.0);
                ui.horizontal(|ui| {
                    let edit = TextEdit::singleline(filter)
                        .hint_text("🔍 Filter...")
                        .desired_width(180.0);
                    let resp = ui.add(edit);
                    if was_opened {
                        resp.request_focus();
                    }
                    if !filter.is_empty() && ui.button("✖").clicked() {
                        filter.clear();
                        resp.request_focus();
                    }
                });
                ui.separator();

                let filter_lower = filter.trim().to_lowercase();
                let matching: Vec<&'a T> = items
                    .iter()
                    .filter(|item| {
                        if filter_lower.is_empty() {
                            true
                        } else {
                            label_fn(item).to_lowercase().contains(&filter_lower)
                        }
                    })
                    .collect();

                if matching.is_empty() {
                    ui.label("(No matches found)");
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for item in matching {
                                let label = label_fn(item);
                                let is_selected = selected_text == label;
                                if ui.selectable_label(is_selected, &label).clicked() {
                                    selected = Some(item);
                                    ui.memory_mut(|mem| mem.close_popup());
                                }
                            }
                        });
                }
            },
        );

        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_input_instantiation() {
        let mut filter = "surge".to_string();
        let _input = SearchInput::new(&mut filter, "Filter...")
            .id_salt("test_salt")
            .desired_width(200.0);
        assert_eq!(filter, "surge");
    }

    #[test]
    fn test_filter_matching() {
        let items = vec!["Surge XT", "Dexed", "Vital", "Surge Synth"];
        let filter = "surge";
        let filter_lower = filter.trim().to_lowercase();
        let matching: Vec<_> = items
            .iter()
            .filter(|item| item.to_lowercase().contains(&filter_lower))
            .copied()
            .collect();
        assert_eq!(matching, vec!["Surge XT", "Surge Synth"]);
    }
}
