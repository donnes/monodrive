//! The General page: LocalSend, updates and the notifications.

use super::*;

impl SettingsView {
    pub(super) fn general(&self, p: &Preferences, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            group(
                "Files",
                None,
                vec![
                    row(
                        "Show hidden files",
                        Some("Files whose names start with a period."),
                        self.focusable(
                            "show-hidden",
                            true,
                            toggle(
                                "show-hidden",
                                "Show hidden files",
                                p.show_hidden,
                                |p, on| p.show_hidden = on,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Sort by",
                        Some("Clicking a column title sorts by it too."),
                        self.focusable(
                            "sort-by",
                            true,
                            segmented(
                                "sort-by",
                                "Sort by",
                                p.sort.by,
                                &[
                                    (SortBy::Name, "Name"),
                                    (SortBy::Modified, "Date"),
                                    (SortBy::Size, "Size"),
                                    (SortBy::Kind, "Kind"),
                                ],
                                |p, by| p.sort.by = by,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Order",
                        None,
                        self.focusable(
                            "sort-order",
                            true,
                            segmented(
                                "sort-order",
                                "Order",
                                p.sort.descending,
                                &[(false, "Ascending"), (true, "Descending")],
                                |p, descending| p.sort.descending = descending,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Keep folders on top",
                        Some("Folders come before files in any order."),
                        self.focusable(
                            "folders-first",
                            true,
                            toggle(
                                "folders-first",
                                "Keep folders on top",
                                p.sort.folders_first,
                                |p, on| p.sort.folders_first = on,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Terminal",
                        Some("What Open in Terminal uses."),
                        self.focusable(
                            "terminal",
                            true,
                            segmented(
                                "terminal",
                                "Terminal",
                                p.terminal,
                                &self
                                    .terminals
                                    .iter()
                                    .map(|terminal| (*terminal, terminal.name()))
                                    .collect::<Vec<_>>(),
                                |p, terminal| p.terminal = terminal,
                            ),
                            cx,
                        ),
                    ),
                ],
            ),
            group("Finder", None, vec![self.show_in_finder(cx)]),
            self.localsend_group(p, cx),
            self.updates_group(p, cx),
            group(
                "Privacy",
                None,
                vec![row(
                    "Share anonymous usage",
                    Some(
                        "Sends PostHog the app and macOS versions with a random ID each launch. Never your files.",
                    ),
                    self.focusable(
                        "share-usage",
                        true,
                        toggle(
                            "share-usage",
                            "Share anonymous usage",
                            p.share_usage,
                            |p, on| p.set_share_usage(on),
                        ),
                        cx,
                    ),
                )],
            ),
        ]
    }

    /// Receiving with LocalSend: whether, where to, and with what PIN. The
    /// LocalSend page's footer has the same switch.
    pub(super) fn localsend_group(&self, p: &Preferences, cx: &mut Context<Self>) -> AnyElement {
        let folder = monodrive_core::paths::abbreviate(&p.localsend_save_to());
        let pin = p.localsend_pin.clone();
        let pin_said = match &pin {
            Some(pin) => format!("Senders type {pin}."),
            None => "Anyone nearby can offer you files.".to_string(),
        };
        let use_localsend = row(
            "Use LocalSend",
            Some("Send files to phones and computers nearby running LocalSend."),
            self.focusable(
                "localsend",
                true,
                toggle("localsend", "Use LocalSend", p.localsend, |p, on| {
                    p.localsend = on
                }),
                cx,
            ),
        );
        if !p.localsend {
            return group("LocalSend", None, vec![use_localsend]).into_any_element();
        }
        group(
            "LocalSend",
            None,
            vec![
                use_localsend,
                row(
                    "Receive files",
                    Some("Devices nearby can offer you files, and you accept each one. They see this Mac by its name in Sharing settings."),
                    self.focusable(
                        "localsend-receive",
                        true,
                        toggle(
                            "localsend-receive",
                            "Receive files",
                            p.localsend_receive,
                            |p, on| p.localsend_receive = on,
                        ),
                        cx,
                    ),
                ),
                row_with(
                    "Save to",
                    Some(folder.into()),
                    self.focusable(
                        "localsend-folder",
                        true,
                        text_button("localsend-folder", "Choose…", ButtonLook::Secondary).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.choose_localsend_folder(window, cx)
                            }),
                        ),
                        cx,
                    ),
                ),
                row_with(
                    "Require a PIN",
                    Some(pin_said.into()),
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .when(pin.is_some(), |d| {
                            d.child(
                                self.focusable(
                                    "localsend-new-pin",
                                    true,
                                    text_button(
                                        "localsend-new-pin",
                                        "New PIN",
                                        ButtonLook::Secondary,
                                    )
                                    .on_click(|_, _, cx| {
                                        preferences::update(cx, |p| {
                                            p.localsend_pin = Some(new_pin())
                                        })
                                    }),
                                    cx,
                                ),
                            )
                        })
                        .child(self.focusable(
                            "localsend-pin",
                            true,
                            toggle("localsend-pin", "Require a PIN", pin.is_some(), |p, on| {
                                p.localsend_pin = on.then(new_pin)
                            }),
                            cx,
                        )),
                ),
            ],
        )
        .into_any_element()
    }

    pub(super) fn choose_localsend_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if let Some(folder) = paths.into_iter().next() {
                let _ = cx
                    .update(|_, cx| preferences::update(cx, |p| p.localsend_folder = Some(folder)));
            }
        })
        .detach();
    }

    /// The switch that sends other apps' Show in Finder here. It shows what
    /// macOS says rather than a saved setting, since another app can take
    /// it at any time.
    pub(super) fn show_in_finder(&self, cx: &mut Context<Self>) -> AnyElement {
        const LABEL: &str = "Make default file browser";
        let on = self
            .finder
            .as_ref()
            .is_some_and(|status| status.viewer == Viewer::Monodrive);
        let enabled = self
            .finder
            .as_ref()
            .is_some_and(|status| status.blocked.is_none());
        let description = self.finder.as_ref().map_or_else(
            || "Other apps' Show in Finder opens here instead of Finder.".into(),
            finder::Status::summary,
        );
        row_with(
            LABEL,
            Some(description.into()),
            self.focusable(
                "show-in-finder",
                enabled,
                theme::switch(on)
                    .id("show-in-finder")
                    .role(Role::Switch)
                    .aria_label(LABEL)
                    .aria_toggled(if on { Toggled::True } else { Toggled::False })
                    .when(!enabled, |d| d.opacity(0.4))
                    .when(enabled, |d| {
                        d.cursor_pointer().on_click(
                            cx.listener(move |_, _, _, cx| Self::ask_finder(Some(!on), cx)),
                        )
                    }),
                cx,
            ),
        )
    }

    pub(super) fn updates_group(&self, p: &Preferences, cx: &mut Context<Self>) -> AnyElement {
        group(
            "Updates",
            None,
            vec![
                row(
                    "Version",
                    Some(monodrive_core::VERSION),
                    self.focusable(
                        "check-updates",
                        true,
                        text_button("check-updates", "Check for Updates", ButtonLook::Secondary)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(CheckForUpdates), cx)
                            }),
                        cx,
                    ),
                ),
                row(
                    "Update track",
                    Some("Which releases the updater offers. Nightly builds come daily."),
                    self.focusable(
                        "update-channel",
                        true,
                        segmented(
                            "update-channel",
                            "Update track",
                            p.update_channel,
                            &[(Channel::Stable, "Stable"), (Channel::Nightly, "Nightly")],
                            |p, channel| p.update_channel = channel,
                        ),
                        cx,
                    ),
                ),
            ],
        )
        .into_any_element()
    }

    /// A notification's switch, dimmed and out of reach while notifications
    /// are off.
    pub(super) fn notify_toggle(
        &self,
        id: &'static str,
        label: &'static str,
        on: bool,
        p: &Preferences,
        set: fn(&mut Preferences, bool),
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let enabled = p.notifications;
        let switch = if enabled {
            toggle(id, label, on, set)
        } else {
            theme::switch(on)
                .id(id)
                .role(Role::Switch)
                .aria_label(label)
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .opacity(0.4)
        };
        self.focusable(id, enabled, switch, cx)
    }

    pub(super) fn allow_notifications(
        &self,
        p: &Preferences,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.focusable(
            "notifications",
            true,
            toggle(
                "notifications",
                "Allow notifications",
                p.notifications,
                |p, on| p.notifications = on,
            ),
            cx,
        )
    }

    pub(super) fn system_notifications(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        self.focusable(
            "notifications-system",
            true,
            text_button(
                "notifications-system",
                "Open System Settings",
                ButtonLook::Secondary,
            )
            .on_click(|_, _, cx| {
                cx.open_url("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
            }),
            cx,
        )
    }

    pub(super) fn worktree_limit(
        &self,
        p: &Preferences,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let limits = WORKTREE_LIMITS
            .into_iter()
            .map(|gb| MenuItem {
                label: format!("{gb} GB").into(),
                icon: None,
                selected: gb == p.notify_worktrees_over,
                choose: Rc::new(move |cx| {
                    preferences::update(cx, |p| p.notify_worktrees_over = gb)
                }),
            })
            .collect();
        div()
            .when(!(p.notifications && p.notify_worktrees), |d| d.opacity(0.4))
            .child(self.popup(
                "notify-worktrees-over",
                "Clean worktrees over",
                80.,
                limits,
                window,
                cx,
            ))
            .into_any_element()
    }

    /// A master switch, then a group per feature.
    pub(super) fn notifications(
        &self,
        p: &Preferences,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        vec![
            group(
                "Notifications",
                None,
                vec![
                    row("Allow notifications", None, self.allow_notifications(p, cx)),
                    row("Banners and sounds", None, self.system_notifications(cx)),
                ],
            ),
            group(
                "LocalSend",
                None,
                vec![
                    row(
                        "Offers from devices",
                        Some("Accept or decline from the notification. Plays AirDrop's chime."),
                        self.notify_toggle(
                            "notify-localsend-offers",
                            "Offers from devices",
                            p.notify_localsend_offers,
                            p,
                            |p, on| p.notify_localsend_offers = on,
                            cx,
                        ),
                    ),
                    row(
                        "Sends and receives that end",
                        Some("Whether everything arrived, or why not."),
                        self.notify_toggle(
                            "notify-localsend-ended",
                            "Sends and receives that end",
                            p.notify_localsend_ended,
                            p,
                            |p, on| p.notify_localsend_ended = on,
                            cx,
                        ),
                    ),
                ],
            ),
            group(
                "Storage",
                Some("Each says so once, then waits a week before saying it again."),
                vec![
                    row(
                        "Startup disk running low",
                        Some("Under 20% free, with what could free space."),
                        self.notify_toggle(
                            "notify-low-disk",
                            "Startup disk running low",
                            p.notify_low_disk,
                            p,
                            |p, on| p.notify_low_disk = on,
                            cx,
                        ),
                    ),
                    row(
                        "Clean worktrees take more than",
                        Some("Worktrees holding nothing their repositories don't."),
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(self.worktree_limit(p, window, cx))
                            .child(self.notify_toggle(
                                "notify-worktrees",
                                "Clean worktrees",
                                p.notify_worktrees,
                                p,
                                |p, on| p.notify_worktrees = on,
                                cx,
                            )),
                    ),
                ],
            ),
        ]
    }
}
