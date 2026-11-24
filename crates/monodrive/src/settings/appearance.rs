//! The Appearance page: the palette, accent, icon and background.

use super::*;

impl SettingsView {
    /// Imports the chosen image off the UI thread, then shows it.
    pub(super) fn choose_background(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let (Some(source), Some(support)) =
                (paths.into_iter().next(), monodrive_core::support_dir())
            else {
                return;
            };
            let imported = cx
                .background_executor()
                .spawn(
                    async move { background::import_image(&source, &support.join("backgrounds")) },
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| match imported {
                Ok(path) => {
                    preferences::update(cx, |p| p.background = Some(path));
                    // Choose… gives way to Change…; keep the keys there.
                    window.focus(&this.handle("change-background", cx), cx);
                }
                Err(error) => {
                    let answer = window.prompt(
                        PromptLevel::Critical,
                        "Monodrive can't use this image",
                        Some(&error),
                        &["OK"],
                        cx,
                    );
                    cx.background_executor()
                        .spawn(async move {
                            let _ = answer.await;
                        })
                        .detach();
                }
            });
        })
        .detach();
    }

    pub(super) fn appearance(
        &self,
        p: &Preferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let light = theme::is_light();
        let swatch_for = |id: &str| -> Rc<dyn Fn() -> AnyElement> {
            let palette = palettes::palette(id, light);
            Rc::new(move || swatch(palette.surface, palette.accent).into_any_element())
        };
        let presets = PALETTES
            .iter()
            .map(|&(id, label, _, _)| MenuItem {
                label: label.into(),
                icon: Some(swatch_for(id)),
                selected: id == p.palette,
                choose: Rc::new(move |cx| preferences::update(cx, |p| p.apply_palette(id))),
            })
            .collect();
        let scales = UI_SCALE_PERCENTS
            .into_iter()
            .map(|percent| MenuItem {
                label: format!("{percent}%").into(),
                icon: None,
                selected: percent == p.ui_scale,
                choose: Rc::new(move |cx| preferences::update(cx, |p| p.ui_scale = percent)),
            })
            .collect();
        vec![
            group(
                "Theme",
                None,
                vec![
                    row(
                        "Appearance",
                        Some("System follows macOS."),
                        self.focusable(
                            "theme",
                            true,
                            segmented(
                                "theme",
                                "Appearance",
                                p.theme,
                                &[
                                    (Theme::System, "System"),
                                    (Theme::Dark, "Dark"),
                                    (Theme::Light, "Light"),
                                ],
                                |p, theme| p.theme = theme,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Color preset",
                        Some("Sets the ink, the accent and the window tint below."),
                        self.popup("palette", "Color preset", 150., presets, window, cx),
                    ),
                    row(
                        "Accent color",
                        Some("Selection, switches and sliders."),
                        self.focusable("accent", true, accent_picker(p, light), cx),
                    ),
                ],
            ),
            group(
                "App Icon",
                None,
                vec![
                    self.focusable(
                        "app-icon",
                        true,
                        app_icon_picker(p.app_icon, window, cx),
                        cx,
                    )
                    .into_any_element(),
                ],
            ),
            group(
                "Color",
                None,
                vec![
                    row(
                        "Hue",
                        Some("Tints the window."),
                        self.slider(Slider::Hue, p, false, cx),
                    ),
                    row(
                        "Saturation",
                        Some("How strongly the hue shows. Zero keeps it neutral."),
                        self.slider(Slider::Saturation, p, false, cx),
                    ),
                    row(
                        "Dark-mode lightness",
                        Some(if light {
                            "Applies in dark mode only."
                        } else {
                            "Lower is darker; zero is black."
                        }),
                        self.slider(Slider::DarkLightness, p, light, cx),
                    ),
                ],
            ),
            group(
                "Translucency",
                Some(if light {
                    "Translucency is available in dark mode."
                } else {
                    "How much of the desktop shows through Monodrive."
                }),
                vec![
                    row(
                        "Window opacity",
                        Some("The sidebar and title bar."),
                        self.slider(Slider::SidebarOpacity, p, light, cx),
                    ),
                    row(
                        "Main pane glass",
                        Some("Lets the file list show the desktop too."),
                        div().when(light, |d| d.opacity(0.4)).child(self.focusable(
                            "main-pane-glass",
                            !light,
                            toggle(
                                "main-pane-glass",
                                "Main pane glass",
                                p.main_pane_glass,
                                |p, on| p.main_pane_glass = on,
                            ),
                            cx,
                        )),
                    ),
                ],
            ),
            self.background_group(p, cx),
            group(
                "Layout",
                None,
                vec![
                    row(
                        "Workspace style",
                        Some("A rounded card or a full-height boxy workspace."),
                        self.focusable(
                            "workspace-style",
                            true,
                            segmented(
                                "workspace-style",
                                "Workspace style",
                                p.workspace_style,
                                &[
                                    (WorkspaceStyle::Rounded, "Rounded"),
                                    (WorkspaceStyle::Boxy, "Boxy"),
                                ],
                                |p, style| p.workspace_style = style,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Interface scale",
                        Some("Zooms the whole window."),
                        self.popup("ui-scale", "Interface scale", 90., scales, window, cx),
                    ),
                ],
            ),
        ]
    }

    pub(super) fn background_group(&self, p: &Preferences, cx: &mut Context<Self>) -> AnyElement {
        const TITLE: &str = "Background";
        const DESCRIPTION: &str = "An image behind the file list. It stays on this Mac.";
        let Some(path) = p.background.clone() else {
            self.preview.update(cx, |preview, cx| preview.set(None, cx));
            return group(
                TITLE,
                Some(DESCRIPTION),
                vec![row(
                    "Image",
                    None,
                    self.focusable(
                        "choose-background",
                        true,
                        text_button("choose-background", "Choose…", ButtonLook::Secondary)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_background(window, cx)
                            })),
                        cx,
                    ),
                )],
            );
        };
        let empty = p.background_empty;
        self.preview.update(cx, |preview, cx| {
            preview.set(
                Some(Look {
                    path,
                    effect: p.background_effect,
                    opacity: empty as f32 / 100.,
                    empty: true,
                    preview: true,
                    radius: theme::RADIUS_CARD - 1.,
                }),
                cx,
            )
        });
        let preview = div()
            .py(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .relative()
                    .h(px(132.))
                    .w_full()
                    .rounded(px(theme::RADIUS_CARD))
                    .border_1()
                    .border_color(content(0.12))
                    .child(self.preview.clone())
                    .child(
                        div()
                            .absolute()
                            .bottom_2()
                            .left_3()
                            .text_size(px(theme::TEXT_CAPTION))
                            .text_color(content(0.65))
                            .child(format!("Empty folder preview at {empty}%")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        self.focusable(
                            "change-background",
                            true,
                            text_button("change-background", "Change…", ButtonLook::Secondary)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_background(window, cx)
                                })),
                            cx,
                        ),
                    )
                    .child(self.focusable(
                        "remove-background",
                        true,
                        text_button("remove-background", "Remove", ButtonLook::Secondary).on_click(
                            cx.listener(|this, _, window, cx| {
                                preferences::update(cx, |p| p.background = None);
                                // Remove goes away with the image; Choose… takes its place.
                                window.focus(&this.handle("choose-background", cx), cx);
                            }),
                        ),
                        cx,
                    )),
            )
            .into_any_element();
        let effects: Vec<_> = Effect::ALL.iter().map(|&e| (e, e.label())).collect();
        group(
            TITLE,
            Some(DESCRIPTION),
            vec![
                preview,
                row(
                    "Background effect",
                    Some(p.background_effect.description()),
                    self.focusable(
                        "background-effect",
                        true,
                        segmented(
                            "background-effect",
                            "Background effect",
                            p.background_effect,
                            &effects,
                            |p, effect| p.background_effect = effect,
                        ),
                        cx,
                    ),
                ),
                row(
                    "Show in",
                    Some("Empty folders only, or every folder."),
                    self.focusable(
                        "background-scope",
                        true,
                        segmented(
                            "background-scope",
                            "Show in",
                            p.background_scope,
                            &[(Scope::Empty, "Empty only"), (Scope::All, "All folders")],
                            |p, scope| p.background_scope = scope,
                        ),
                        cx,
                    ),
                ),
                row(
                    "Empty folder visibility",
                    Some("Background strength in a folder with nothing in it."),
                    self.slider(Slider::BackgroundEmpty, p, false, cx),
                ),
                row(
                    "Folder visibility",
                    Some("Background strength behind a folder's items."),
                    self.slider(
                        Slider::BackgroundFolder,
                        p,
                        p.background_scope == Scope::Empty,
                        cx,
                    ),
                ),
            ],
        )
    }
}
