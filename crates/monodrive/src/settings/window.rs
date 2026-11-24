//! The Window page: how the window looks and opens, and the drives it hides.

use super::*;

impl SettingsView {
    pub(super) fn window_page(&self, p: &Preferences, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            group(
                "Window",
                None,
                vec![row(
                    "Default size",
                    Some("Reasonable is 70% of the screen and Almost Full 90%, centered."),
                    self.focusable(
                        "window-size",
                        true,
                        segmented(
                            "window-size",
                            "Default size",
                            p.window_size,
                            &[
                                (WindowSize::Reasonable, "Reasonable"),
                                (WindowSize::AlmostFull, "Almost Full"),
                                (WindowSize::LastUsed, "Last Used"),
                            ],
                            |p, size| p.window_size = size,
                        )
                        // Picking the size already chosen restores it after
                        // a manual resize; a new choice resizes once more,
                        // to the same frame.
                        .on_click(|_, window, cx| {
                            frame::apply(cx.global::<Preferences>().window_size, window, cx)
                        }),
                        cx,
                    ),
                )],
            ),
            group(
                "View",
                None,
                vec![
                    row(
                        "View as",
                        Some("⌥⌘1 to ⌥⌘3 switch it too."),
                        self.focusable(
                            "view",
                            true,
                            segmented(
                                "view",
                                "View as",
                                p.view,
                                &[
                                    (View::List, "List"),
                                    (View::Grid, "Grid"),
                                    (View::Columns, "Columns"),
                                ],
                                |p, view| p.view = view,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Show path bar",
                        Some(
                            "The path, selection and free space under the files. ⌥⌘P switches it too.",
                        ),
                        self.focusable(
                            "show-path-bar",
                            true,
                            toggle(
                                "show-path-bar",
                                "Show path bar",
                                p.show_path_bar,
                                |p, on| p.show_path_bar = on,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Open folders while dragging",
                        Some("Resting on a folder for a moment opens it. Space opens it at once."),
                        self.focusable(
                            "open-folders-while-dragging",
                            true,
                            toggle(
                                "open-folders-while-dragging",
                                "Open folders while dragging",
                                p.open_folders_while_dragging,
                                |p, on| p.open_folders_while_dragging = on,
                            ),
                            cx,
                        ),
                    ),
                ],
            ),
            group(
                "Sidebar",
                None,
                std::iter::once(row(
                    "Show Worktrees",
                    Some("Every Git worktree on the disk, in one place. ⌥⌘G opens it either way."),
                    self.focusable(
                        "show-worktrees",
                        true,
                        toggle(
                            "show-worktrees",
                            "Show Worktrees",
                            p.show_worktrees,
                            |p, on| p.show_worktrees = on,
                        ),
                        cx,
                    ),
                ))
                .chain(self.hidden_drives(p, cx))
                .collect(),
            ),
            group(
                "Grid",
                None,
                vec![
                    row(
                        "Icon size",
                        None,
                        self.slider(Slider::GridIcon, p, false, cx),
                    ),
                    row(
                        "Label size",
                        None,
                        self.focusable(
                            "grid-label",
                            true,
                            segmented(
                                "grid-label",
                                "Label size",
                                p.grid_label,
                                &[
                                    (RowSize::Small, "Small"),
                                    (RowSize::Medium, "Medium"),
                                    (RowSize::Large, "Large"),
                                ],
                                |p, size| p.grid_label = size,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Show thumbnails",
                        Some("Images and videos show a picture instead of an icon."),
                        self.focusable(
                            "thumbnails",
                            true,
                            toggle("thumbnails", "Show thumbnails", p.thumbnails, |p, on| {
                                p.thumbnails = on
                            }),
                            cx,
                        ),
                    ),
                    row(
                        "Suggest names in sentence case",
                        Some("Invoice 4471.pdf instead of invoice-4471.pdf."),
                        self.focusable(
                            "sentence-case-names",
                            true,
                            toggle(
                                "sentence-case-names",
                                "Suggest names in sentence case",
                                p.sentence_case_names,
                                |p, on| p.sentence_case_names = on,
                            ),
                            cx,
                        ),
                    ),
                ],
            ),
            group(
                "List",
                None,
                vec![row(
                    "Row size",
                    Some("Icons and text grow with the rows."),
                    self.focusable(
                        "row-size",
                        true,
                        segmented(
                            "row-size",
                            "Row size",
                            p.row_size,
                            &[
                                (RowSize::Small, "Small"),
                                (RowSize::Medium, "Medium"),
                                (RowSize::Large, "Large"),
                            ],
                            |p, size| p.row_size = size,
                        ),
                        cx,
                    ),
                )],
            ),
        ]
    }

    /// A row for each drive Hide from Sidebar took out of Locations, with
    /// the button that puts it back. Focus stays on the row that takes a
    /// shown one's place, or the control above when the last one goes.
    pub(super) fn hidden_drives(&self, p: &Preferences, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let last = p.hidden_drives.len().saturating_sub(1);
        (p.hidden_drives.iter().enumerate())
            .map(|(ix, hidden)| {
                let drive = hidden.drive.clone();
                let button = text_button(("show-drive", ix), "Show", ButtonLook::Secondary)
                    .debug_selector(move || format!("show-drive-{ix}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let id = format!("show-drive-{ix}");
                        if ix == last && this.handle(id, cx).is_focused(window) {
                            let above = match ix {
                                0 => "show-worktrees".into(),
                                ix => format!("show-drive-{}", ix - 1),
                            };
                            window.focus(&this.handle(above, cx), cx);
                            this.reveal(window);
                        }
                        preferences::update(cx, |p| p.hidden_drives.retain(|h| h.drive != drive))
                    }));
                row_with(
                    hidden.name.clone(),
                    Some("Hidden from Locations".into()),
                    self.focusable(format!("show-drive-{ix}"), true, button, cx),
                )
            })
            .collect()
    }
}
