//! The Keyboard page: every command's shortcut, and recording a new one.

use super::*;

impl SettingsView {
    /// Every command and its shortcut, grouped as in the menu bar.
    pub(super) fn keyboard(&self, p: &Preferences, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let all: Vec<_> = commands::all()
            .into_iter()
            .filter(|command| !commands::hidden(command, p))
            .collect();
        let mut page = Vec::new();
        if let Some(error) = &self.recording_error {
            page.push(
                div()
                    .px_1()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(theme::danger())
                    .child(error.clone())
                    .into_any_element(),
            );
        }
        for group_of in Group::ALL {
            let rows = all
                .iter()
                .filter(|command| command.group == group_of)
                .map(|command| {
                    let id = command.id();
                    let context = command.context;
                    let recording = self.recording == Some(id);
                    let changed = p.shortcuts.contains_key(id);
                    let keys = command.shortcuts(&p.shortcuts);
                    let shown = if recording {
                        "Type a shortcut…".to_string()
                    } else if keys.is_empty() {
                        "None".to_string()
                    } else {
                        keys.iter()
                            .map(|keys| commands::label(keys))
                            .collect::<Vec<_>>()
                            .join("  ")
                    };
                    let button = theme::button(id, ButtonLook::Secondary)
                        .aria_label(format!("{}: {shown}", command.title))
                        .min_w(px(96.))
                        .border_1()
                        .border_color(if recording {
                            accent()
                        } else {
                            gpui::transparent_black()
                        })
                        .when(recording, |d| d.bg(accent().opacity(0.2)))
                        .when(keys.is_empty() && !recording, |d| {
                            d.text_color(content(0.65))
                        })
                        .child(shown)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_recording(id, context, window, cx)
                        }));
                    row(
                        command.title,
                        None,
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(changed, |d| {
                                d.child(
                                    theme::tip(
                                        theme::row_icon_button(
                                            SharedString::from(format!("reset-{id}")),
                                            "Use the default",
                                            "rotate-ccw",
                                            content(0.55),
                                        ),
                                        "Use the default",
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.reset_shortcut(id, context, cx),
                                    )),
                                )
                            })
                            .child(self.focusable(id, true, button, cx)),
                    )
                })
                .collect();
            page.push(group(group_of.label(), None, rows));
        }
        page
    }

    /// Records the next key press as `id`'s shortcut, ahead of everything
    /// that would otherwise run it: ⌘, records rather than opening Settings.
    pub(super) fn start_recording(
        &mut self,
        id: &'static str,
        context: Option<&'static str>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.recording = Some(id);
        self.recording_error = None;
        let this = cx.weak_entity();
        self.recorder = Some(cx.intercept_keystrokes(move |event, _, cx| {
            let keystroke = event.keystroke.clone();
            let m = keystroke.modifiers;
            // Tab gives up and moves on, so recording never traps the keyboard.
            let tab = keystroke.key == "tab" && !(m.platform || m.control || m.alt);
            if !tab {
                cx.stop_propagation();
            }
            let _ = this.update(cx, |this, cx| {
                if tab {
                    this.recording_error = None;
                    this.stop_recording(cx);
                } else {
                    this.record(id, context, &keystroke, cx);
                }
            });
        }));
        cx.notify();
    }

    /// Puts `id` back on its default keys, unless another command has taken
    /// one of them since.
    pub(super) fn reset_shortcut(
        &mut self,
        id: &'static str,
        context: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let all = commands::all();
        let Some(command) = all.iter().find(|command| command.id() == id) else {
            return;
        };
        let taken = command.keys.iter().find_map(|keys| {
            let stroke = gpui::Keystroke::parse(keys).ok()?;
            let other = commands::conflict(&stroke, id, context, cx)?;
            Some(format!(
                "{} is already used by {other}.",
                commands::label(keys)
            ))
        });
        match taken {
            Some(error) => self.recording_error = Some(error),
            None => {
                self.recording_error = None;
                preferences::update(cx, |p| {
                    p.shortcuts.remove(id);
                });
            }
        }
        cx.notify();
    }

    pub(super) fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.recorder = None;
        cx.notify();
    }

    pub(super) fn record(
        &mut self,
        id: &'static str,
        context: Option<&'static str>,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) {
        let m = keystroke.modifiers;
        let key = keystroke.key.as_str();
        match key {
            "escape" if !m.modified() => {
                self.recording_error = None;
                self.stop_recording(cx);
                return;
            }
            "backspace" | "delete" if !m.modified() => {
                preferences::update(cx, |p| {
                    p.shortcuts.insert(id.to_string(), None);
                });
                self.recording_error = None;
                self.stop_recording(cx);
                return;
            }
            _ => {}
        }
        let function_key = key.len() > 1 && key.starts_with('f') && key[1..].parse::<u8>().is_ok();
        if !(m.platform || m.control || m.alt || function_key) {
            self.recording_error = Some("Use ⌘, ⌃ or ⌥ with a key, or a function key.".into());
            cx.notify();
            return;
        }
        let keys = keystroke.unparse();
        if let Some(other) = commands::conflict(keystroke, id, context, cx) {
            self.recording_error = Some(format!(
                "{} is already used by {other}.",
                commands::label(&keys)
            ));
            cx.notify();
            return;
        }
        preferences::update(cx, |p| {
            p.shortcuts.insert(id.to_string(), Some(keys));
        });
        self.recording_error = None;
        self.stop_recording(cx);
    }
}
