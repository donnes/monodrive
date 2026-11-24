//! The row of tools under the picture: the tools, inks, the text card, Save and its menu.

use super::*;

impl PreviewView {
    /// The Text tool's card, beside a dot where the words will land. It's
    /// modal: a press elsewhere closes an empty card and can't lose written
    /// words, and nothing behind it reacts.
    pub(super) fn note_card(
        &self,
        source: (f32, f32),
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let edit = self.edit.as_ref()?;
        let (field, at, _) = edit.text.as_ref()?;
        let field = field.clone();
        let surface = edit.drawn.get();
        let (origin, scale) = fitted_in(surface, source, edit.zoom);
        let dot = origin + point(px(at[0] as f32 * scale), px(at[1] as f32 * scale));
        let width = px(NOTE_WIDTH).min(surface.size.width - px(16.));
        let left = (dot.x + px(12.))
            .min(surface.right() - width - px(8.))
            .max(surface.left() + px(8.));
        let top = (dot.y + px(12.))
            .min(surface.bottom() - px(NOTE_HEIGHT) - px(8.))
            .max(surface.top() + px(8.));
        let empty = field.read(cx).text().trim().is_empty();
        let rewriting = edit.rewriting.is_some();
        let label = if rewriting { "Done" } else { "Add" };
        let add = if empty {
            text_button("note-add", label, ButtonLook::Disabled)
        } else {
            text_button("note-add", label, ButtonLook::Primary)
                .on_click(cx.listener(|this, _, window, cx| this.commit_text(window, cx)))
        };
        let card = theme::floating(theme::Elevation::Popover)
            .debug_selector(|| "note-card".into())
            .absolute()
            .left(left)
            .top(top)
            .w(width)
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .occlude()
            .key_context("Rename")
            .on_action(
                cx.listener(|this, _: &crate::commands::CommitRename, window, cx| {
                    this.commit_text(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::commands::CancelRename, window, cx| {
                    this.cancel_text(window, cx)
                }),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_BODY))
                    .font_weight(FontWeight::MEDIUM)
                    .child(if rewriting { "Change text" } else { "Add text" }),
            )
            .child(
                theme::field(theme::accent())
                    .debug_selector(|| "note-field".into())
                    .rounded(px(theme::RADIUS_ROW))
                    .p_2()
                    .child(field.clone()),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        text_button("note-cancel", "Cancel", ButtonLook::Ghost)
                            .debug_selector(|| "note-cancel".into())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.cancel_text(window, cx)),
                            ),
                    )
                    .child(add.debug_selector(|| "note-add".into())),
            );
        let viewport = window.viewport_size();
        Some(deferred(
            gpui::anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("note-scrim")
                    .debug_selector(|| "note-scrim".into())
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if field.read(cx).text().trim().is_empty() {
                                this.cancel_text(window, cx);
                            }
                        }),
                    )
                    .when(!rewriting, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(dot.x - px(5.))
                                .top(dot.y - px(5.))
                                .size(px(10.))
                                .rounded_full()
                                // design: a mark's own color, part of the file and not the app.
                                .bg(hsla(edit.ink))
                                .border_1()
                                .border_color(content(1.)),
                        )
                    })
                    .child(card),
            ),
        ))
    }

    /// A tool: a 24 pt icon, named when hovered.
    pub(super) fn tool_button(
        &self,
        id: &'static str,
        label: &'static str,
        path: &'static str,
        on: bool,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<gpui::Div> {
        theme::tip(theme::icon_button(id, label, path, true), label)
            .debug_selector(move || id.into())
            .when(on, |d| d.bg(content(0.14)))
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(edit) = &mut this.edit {
                    edit.menu = None;
                }
                click(this, window, cx)
            }))
    }

    pub(super) fn inks(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let edit = self.edit.as_ref()?;
        let ink = edit.ink;
        matches!(edit.subject, Subject::Picture { .. }).then(|| {
            div()
                .flex()
                .gap_1()
                .items_center()
                .children(INKS.map(|(id, color)| {
                    div()
                        .id(id)
                        .debug_selector(move || id.into())
                        .size(px(18.))
                        .rounded_full()
                        .cursor_pointer()
                        // design: a mark's own color, part of the file and not the app.
                        .bg(hsla(color))
                        // An outline, so black shows on a dark pane.
                        .border_1()
                        .border_color(content(0.3))
                        .when(ink == color, |d| d.border_2().border_color(content(1.)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let mut recolored = false;
                            if let Some(edit) = &mut this.edit {
                                edit.ink = color;
                                edit.menu = None;
                                if let Some(index) = edit.selected {
                                    let new = edit.steps().get(index).map(|m| m.in_color(color));
                                    if new.as_ref() != edit.steps().get(index) {
                                        edit.remember_steps();
                                        if let Subject::Picture { steps, .. } = &mut edit.subject
                                            && let (Some(slot), Some(new)) =
                                                (steps.get_mut(index), new)
                                        {
                                            *slot = new;
                                            recolored = true;
                                        }
                                    }
                                }
                            }
                            if recolored {
                                this.flatten(window, cx);
                            }
                            cx.notify();
                        }))
                }))
        })
    }

    /// What this file can have done to it, as icons.
    pub(super) fn tool_buttons(&mut self, cx: &mut Context<Self>) -> Vec<Stateful<gpui::Div>> {
        let Some(edit) = self.edit.as_ref() else {
            return Vec::new();
        };
        let tool = edit.tool;
        let mut tools = Vec::new();
        match &edit.subject {
            Subject::Picture { .. } => {
                tools.push(self.tool_button(
                    "tool-select",
                    "Move",
                    "tool-select",
                    tool.is_none(),
                    cx,
                    |this, _, cx| {
                        if let Some(edit) = &mut this.edit {
                            edit.tool = None;
                        }
                        cx.notify();
                    },
                ));
                for (kind, id, label, path) in PICTURE_TOOLS {
                    tools.push(self.tool_button(
                        id,
                        label,
                        path,
                        tool == Some(kind),
                        cx,
                        move |this, _, cx| {
                            if kind == Tool::Crop {
                                return this.start_crop(cx);
                            }
                            if let Some(edit) = &mut this.edit {
                                edit.tool = (edit.tool != Some(kind)).then_some(kind);
                                edit.selected = None;
                            }
                            cx.notify();
                        },
                    ));
                }
                tools.push(self.tool_button(
                    "tool-rotate",
                    "Rotate",
                    "tool-rotate",
                    false,
                    cx,
                    |this, window, cx| {
                        this.add_step(Step::Rotate { quarter_turns: 1 }, window, cx);
                    },
                ));
                tools.push(self.tool_button(
                    "tool-flip",
                    "Flip",
                    "tool-flip",
                    false,
                    cx,
                    |this, window, cx| {
                        this.add_step(Step::Flip { vertical: false }, window, cx);
                    },
                ));
            }
            Subject::Video {
                start,
                end,
                muted,
                turns,
                crop,
                ..
            } => {
                let (start, end, muted, turns, cropped) =
                    (*start, *end, *muted, *turns, crop.is_some());
                tools.push(self.tool_button(
                    "tool-trim",
                    "Trim",
                    "tool-trim",
                    start.is_some() || end.is_some(),
                    cx,
                    |this, window, cx| this.start_trim(window, cx),
                ));
                tools.push(self.tool_button(
                    "tool-video-crop",
                    "Crop",
                    "tool-crop",
                    tool == Some(Tool::Crop) || cropped,
                    cx,
                    |this, _, cx| {
                        if this.cropping() {
                            this.cancel_crop(cx);
                        } else {
                            this.freeze_for_crop(cx);
                        }
                    },
                ));
                tools.push(self.tool_button(
                    "tool-video-rotate",
                    "Rotate",
                    "tool-rotate",
                    turns != 0,
                    cx,
                    |this, _, cx| {
                        if let Some(edit) = &mut this.edit {
                            edit.remember();
                            edit.turn_video();
                        }
                        cx.notify();
                    },
                ));
                tools.push(self.tool_button(
                    "tool-video-mute",
                    "Remove Sound",
                    "tool-mute",
                    muted,
                    cx,
                    |this, _, cx| {
                        if let Some(edit) = &mut this.edit {
                            edit.remember();
                            if let Subject::Video { muted, .. } = &mut edit.subject {
                                *muted = !*muted;
                            }
                        }
                        cx.notify();
                    },
                ));
                tools.push(self.tool_button(
                    "tool-frame",
                    "Save Frame",
                    "tool-frame",
                    false,
                    cx,
                    |this, window, cx| {
                        this.save_frame(window, cx);
                    },
                ));
            }
        }
        tools
    }

    /// Save, which makes a copy and can't touch the original, and an arrow for
    /// the two other ways to save.
    pub(super) fn save_control(
        &self,
        changed: bool,
        open: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let save = if changed {
            text_button("edit-save", "Save", ButtonLook::Primary)
                .debug_selector(|| "edit-save".into())
                .on_click(cx.listener(|this, _, window, cx| this.save(SaveTo::Copy, window, cx)))
        } else {
            text_button("edit-save", "Save", ButtonLook::Disabled)
        };
        // One button split in two: only the outer ends are round.
        // design: the inner corners of a split button meet square.
        let save = save.rounded_r_none().pr(px(8.));
        let more = theme::button(
            "edit-more",
            if changed {
                ButtonLook::Primary
            } else {
                ButtonLook::Disabled
            },
        )
        // design: the inner corners of a split button meet square.
        .rounded_l_none()
        .px(px(5.))
        .child(theme::icon("chevron-down-hi", 14.).text_color(if changed {
            theme::on_accent()
        } else {
            content(0.3)
        }));
        let more = if changed {
            more.debug_selector(|| "edit-more".into())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Save, cx)))
        } else {
            more
        };
        div()
            .relative()
            .flex_shrink_0()
            .flex()
            .gap(px(1.))
            .child(save)
            .child(more)
            .when(open, |d| {
                let row = |id: &'static str, label: &'static str| {
                    theme::row(false)
                        .id(id)
                        .debug_selector(move || id.into())
                        .px_2()
                        .h(px(theme::ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .text_size(px(theme::TEXT_BODY))
                        .cursor_pointer()
                        // The pointer's row, as menus show it.
                        .hover(|style| style.bg(theme::accent()).text_color(theme::on_accent()))
                        .child(label)
                };
                d.child(deferred(
                    menu_card(200., true)
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
                        // The gap every menu keeps between its rows.
                        .flex()
                        .flex_col()
                        .gap(px(theme::ROW_GAP))
                        .child(
                            row("edit-replace", "Replace Original").on_click(cx.listener(
                                |this, _, window, cx| this.save(SaveTo::Replace, window, cx),
                            )),
                        )
                        .child(
                            row("edit-save-as", "Save As…").on_click(
                                cx.listener(|this, _, window, cx| this.save_as(window, cx)),
                            ),
                        ),
                ))
            })
    }

    /// The title row while editing: tools at the left, Undo, Save and Done at the
    /// right. A narrow pane moves the tools behind a menu, so nothing wraps.
    pub(in crate::preview) fn edit_row(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let width = self.row_width.get();
        if let Some(edit) = &mut self.edit {
            // A pane that grew has no Tools button left to hold this menu open.
            let picture = matches!(edit.subject, Subject::Picture { .. });
            if edit.menu == Some(Menu::Tools) && !row_fits(width, picture).0 {
                edit.menu = None;
            }
        }
        let edit = self.edit.as_ref()?;
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let saving = edit.saving.clone();
        let changed = edit.changed();
        let confirming = edit.confirming;
        let menu = edit.menu;
        let picture = matches!(edit.subject, Subject::Picture { .. });
        let edit_selected = edit.selected.is_some();
        let cropping = edit.cropping.as_ref().map(|cropping| cropping.rect);
        let trimming = edit.trimming.as_ref().map(|trimming| {
            let (start, end) = (clock_tenths(trimming.start), clock_tenths(trimming.end));
            match trimming.grab.map(|(grab, _)| grab) {
                Some(TrimGrab::Start) => start,
                Some(TrimGrab::End) => end,
                _ => format!("{start} – {end}"),
            }
        });
        let row_width = self.row_width.clone();
        let (narrow, very_narrow) = row_fits(row_width.get(), picture);
        // Shrinks before anything else; the cap keeps room for the status.
        let name_cell = div()
            .debug_selector(|| "preview-name".into())
            .min_w_0()
            .max_w(relative(0.4))
            .flex_shrink(1.)
            .truncate()
            .text_size(px(theme::TEXT_BODY))
            .font_weight(FontWeight::MEDIUM)
            .child(name);
        let measure = canvas(
            move |bounds, window, _| {
                let width = bounds.size.width.as_f32();
                // A width the last frame guessed wrong lays out again next frame.
                if row_fits(width, picture) != row_fits(row_width.get(), picture) {
                    window.on_next_frame(|window, _| window.refresh());
                }
                row_width.set(width);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let row = theme::pane_bar(theme::TITLE_BAR_HEIGHT)
            .debug_selector(|| "preview-tools".into())
            .relative()
            .gap_1()
            .flex_shrink_0()
            .border_b_1()
            .border_color(content(0.06))
            .child(measure);
        let separator = || {
            div()
                .flex_shrink_0()
                .w(px(1.))
                .h(px(16.))
                .mx_1()
                .bg(content(0.12))
        };
        let status = |text: gpui::SharedString| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child(text)
        };
        let row = if let Some(cancel) = saving {
            row.child(name_cell)
                .child(separator())
                .child(status("Saving…".into()))
                .child(
                    text_button("edit-cancel", "Cancel", ButtonLook::Secondary)
                        .debug_selector(|| "edit-cancel".into())
                        .on_click(move |_, _, _| cancel.store(true, Ordering::Relaxed)),
                )
        } else if confirming {
            row.child(name_cell)
                .child(separator())
                .child(status("Discard these changes?".into()))
                .child(
                    text_button("edit-keep", "Keep Editing", ButtonLook::Ghost)
                        .debug_selector(|| "edit-keep".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(edit) = &mut this.edit {
                                edit.confirming = false;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    text_button("edit-discard", "Discard", ButtonLook::Primary)
                        .debug_selector(|| "edit-discard".into())
                        .on_click(cx.listener(|this, _, _, cx| this.leave_edit(cx))),
                )
        } else if let Some(times) = trimming {
            row.child(name_cell)
                .child(separator())
                .child(status(times.into()))
                .child(
                    text_button("trim-cancel", "Cancel", ButtonLook::Ghost)
                        .debug_selector(|| "trim-cancel".into())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_trim(cx))),
                )
                .child(
                    text_button("trim-apply", "Trim", ButtonLook::Primary)
                        .debug_selector(|| "trim-apply".into())
                        .on_click(cx.listener(|this, _, _, cx| this.apply_trim(cx))),
                )
        } else if let Some(rect) = cropping {
            row.child(name_cell)
                .child(separator())
                .child(status(
                    format!(
                        "{} × {} · Drag the edges or the middle",
                        rect.width.round(),
                        rect.height.round()
                    )
                    .into(),
                ))
                .child(
                    text_button("edit-crop-cancel", "Cancel", ButtonLook::Ghost)
                        .debug_selector(|| "edit-crop-cancel".into())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_crop(cx))),
                )
                .child(
                    text_button("edit-crop-apply", "Crop", ButtonLook::Primary)
                        .debug_selector(|| "edit-crop-apply".into())
                        .on_click(cx.listener(|this, _, window, cx| this.apply_crop(window, cx))),
                )
        } else {
            let tools = self.tool_buttons(cx);
            let inks = self.inks(cx);
            let delete = edit_selected.then(|| {
                self.tool_button(
                    "edit-delete",
                    "Delete",
                    "trash",
                    false,
                    cx,
                    |this, window, cx| {
                        this.delete_mark(window, cx);
                    },
                )
            });
            let undo = self.tool_button(
                "edit-undo",
                "Undo",
                "tool-undo",
                false,
                cx,
                |this, window, cx| this.undo_step(window, cx),
            );
            let save = self.save_control(changed, menu == Some(Menu::Save), cx);
            let done = text_button("edit-done", "Done", ButtonLook::Ghost)
                .debug_selector(|| "edit-done".into())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_edit(window, cx)));
            let row = if very_narrow {
                row
            } else {
                row.child(name_cell).child(separator())
            };
            let row = if narrow {
                let open = menu == Some(Menu::Tools);
                let more = theme::button("edit-tools", ButtonLook::Ghost)
                    .debug_selector(|| "edit-tools".into())
                    .when(open, |d| d.bg(content(0.14)))
                    .child("Tools")
                    .child(theme::icon("chevron-down-hi", 12.))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Tools, cx)));
                row.child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .child(more)
                        .when(open, |d| {
                            d.child(deferred(
                                menu_card(232., false)
                                    .on_mouse_down_out(
                                        cx.listener(|this, _, _, cx| this.close_menu(cx)),
                                    )
                                    .child(div().flex().flex_wrap().gap_1().p_2().children(tools))
                                    .children(inks.map(|inks| div().px_2().pb_2().child(inks))),
                            ))
                        }),
                )
            } else {
                row.children(tools)
                    .when(inks.is_some(), |d| d.child(separator()))
                    .children(inks)
            };
            row.child(div().flex_1().min_w_0())
                .children(delete)
                .child(undo)
                .child(save)
                .child(done)
        };
        Some(row.into_any_element())
    }

    /// A failed save's reason, under the row, until the next change.
    pub(in crate::preview) fn edit_error(&self) -> Option<impl IntoElement + use<>> {
        let why = self.edit.as_ref()?.error.clone()?;
        Some(
            theme::pane_bar(theme::PATH_BAR_HEIGHT)
                .debug_selector(|| "edit-status".into())
                .flex_shrink_0()
                .border_b_1()
                .border_color(content(0.06))
                .text_size(px(theme::TEXT_SMALL))
                .text_color(danger())
                .child(div().min_w_0().truncate().child(why)),
        )
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.menu = None;
        }
        cx.notify();
    }

    pub(super) fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.menu = (edit.menu != Some(menu)).then_some(menu);
        }
        cx.notify();
    }

    /// A menu is open, which a player would otherwise draw over.
    pub(in crate::preview) fn menu_open(&self) -> bool {
        self.edit.as_ref().is_some_and(|edit| edit.menu.is_some())
    }

    pub(super) fn save_frame(&mut self, window: &Window, cx: &mut Context<Self>) {
        let seconds = self
            .player
            .borrow()
            .as_ref()
            .map_or(0., |player| player.position());
        let Some(source) = self.edit.as_ref().and_then(|edit| edit.opened.clone()) else {
            return;
        };
        let Some(ops) = cx.try_global::<Operations>().map(|ops| ops.0.clone()) else {
            return;
        };
        let path: PathBuf = self.path.clone();
        let Some(folder) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        let prepared = edits::prepared_path(&folder, "png");
        let work = cx.background_executor().spawn(async move {
            let done = edits::frame(&path, seconds, &prepared).and_then(|()| {
                let request = Request::SaveEdit {
                    source,
                    prepared: prepared.clone(),
                    save: SaveTo::Copy,
                };
                let outcome = crate::lock(&ops).submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            });
            edits::discard(&prepared);
            done
        });
        cx.spawn_in(window, async move |this, cx| {
            let done = work.await;
            let _ = this.update(cx, |_, cx| cx.emit(Saved(done)));
        })
        .detach();
    }
}
