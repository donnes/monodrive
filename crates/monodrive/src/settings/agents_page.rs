//! The Agents page: which coding agents get `monodrive mcp`, and adding it.

use super::*;

impl SettingsView {
    /// Reads the agents' configs off the UI thread to find the ones to offer
    /// and mark those that already have Monodrive.
    pub(super) fn check_agents(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (installed, added) = cx
                .background_executor()
                .spawn(async { (agents::detected(), agents::added()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.harness = keep_or_first(this.harness, &installed);
                this.installed = Some(installed);
                this.added = added;
                cx.notify();
            });
        })
        .detach();
    }

    /// Adds Monodrive to `harness`'s config or removes it, off the UI thread.
    pub(super) fn apply(&mut self, harness: Harness, action: Action, cx: &mut Context<Self>) {
        if self.job.as_ref().is_some_and(|job| job.failed.is_none()) {
            return;
        }
        self.job = Some(Job {
            harness,
            action,
            failed: None,
        });
        cx.notify();
        let exe = agents::exe();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let home = agents::home().ok_or("HOME isn't set.")?;
                    harness.apply(action, &exe, &home)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.job = None;
                        this.over_action = false;
                        // Show the result now; the check confirms it from disk.
                        this.added.retain(|h| *h != harness);
                        if action == Action::Add {
                            this.added.push(harness);
                        }
                        this.check_agents(cx);
                    }
                    Err(error) => {
                        if let Some(job) = &mut this.job {
                            job.failed = Some(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Add button, which shows how the last Add or Remove is going.
    pub(super) fn action_button(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let harness = self.harness;
        let job = self.job.as_ref().filter(|job| job.harness == harness);
        let added = self.added.contains(&harness);
        let id = "agent-action";
        let keyboard = self.handle(id, cx).is_focused(window) && window.last_input_was_keyboard();
        let frame = |look| {
            theme::button(id, look)
                .min_w(px(112.))
                .on_hover(cx.listener(|this, over: &bool, _, cx| {
                    this.over_action = *over;
                    cx.notify();
                }))
        };
        let (button, next) = match job {
            Some(Job {
                action,
                failed: None,
                ..
            }) => {
                let label = match action {
                    Action::Add => "Adding…",
                    Action::Remove => "Removing…",
                };
                (
                    frame(ButtonLook::Disabled).aria_label(label).child(label),
                    None,
                )
            }
            Some(Job {
                action,
                failed: Some(_),
                ..
            }) => {
                let button = frame(ButtonLook::Caution)
                    .aria_label("Try Again")
                    .child("Try Again");
                (button, Some(*action))
            }
            None if added => {
                let remove = self.over_action || keyboard;
                let label = if remove { "Remove" } else { "Added" };
                let button = frame(ButtonLook::Secondary)
                    .aria_label(format!("Remove from {}", harness.name()))
                    .when(!remove, |d| {
                        d.child(icon("check-hi", 12.).text_color(theme::success()))
                    })
                    .child(label);
                (button, remove.then_some(Action::Remove))
            }
            None => {
                let button = frame(ButtonLook::Primary)
                    .aria_label(format!("Add to {}", harness.name()))
                    .child(format!("Add to {}", harness.name()));
                (button, Some(Action::Add))
            }
        };
        let button = match next {
            Some(action) => {
                button.on_click(cx.listener(move |this, _, _, cx| this.apply(harness, action, cx)))
            }
            None => button,
        };
        let running = job.is_some_and(|job| job.failed.is_none());
        self.focusable(id, !running, button, cx).into_any_element()
    }

    pub(super) fn pick_harness(&mut self, harness: Harness, cx: &mut Context<Self>) {
        self.harness = harness;
        self.copied = None;
        cx.notify();
    }

    /// Text in a code box with a Copy button that reads Copied once pressed.
    pub(super) fn code_box(
        &self,
        id: &'static str,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let copied = self.copied == Some(id);
        let copy = text.clone();
        let label = if copied { "Copied" } else { "Copy" };
        let button = theme::button(id, ButtonLook::Secondary)
            .aria_label(label)
            .when(copied, |d| {
                d.child(icon("check-hi", 12.).text_color(content(0.65)))
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                this.copied = Some(id);
                cx.notify();
            }));
        div()
            .w_full()
            .min_h(px(36.))
            .py(px(6.))
            .pl_3()
            .pr(px(6.))
            .rounded(px(theme::RADIUS_CARD))
            .bg(theme::well())
            .border_1()
            .border_color(content(0.06))
            .flex()
            .items_start()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pt(px(4.))
                    .font_family(theme::MONO_FONT)
                    .text_size(px(theme::TEXT_CAPTION))
                    .line_height(px(16.))
                    .child(text),
            )
            .child(self.focusable(id, true, button, cx))
            .into_any_element()
    }

    pub(super) fn agents(
        &self,
        p: &Preferences,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let exe = agents::exe();
        let harness = self.harness;
        let (step, code) = match harness.setup(&exe) {
            Setup::Command(command) => ("Run in a terminal".to_string(), command),
            Setup::Config { file, json } => (format!("Add to {file}"), json),
        };
        let server = div()
            .py(px(12.))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(content(0.08))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("bot", 16.).text_color(content(1.))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_BODY))
                                    .child("Local MCP server"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_CAPTION))
                                    .line_height(px(16.))
                                    .text_color(content(0.65))
                                    .child(
                                        "Works with any agent that runs MCP servers over stdio. \
                                         Each agent session starts its own.",
                                    ),
                            ),
                    ),
            )
            .child(self.code_box(
                "copy-server",
                format!("{} mcp", agents::shell_quote(&exe)),
                cx,
            ));
        let harnesses = self.installed.clone().unwrap_or_default();
        let at = harnesses.iter().position(|h| *h == harness).unwrap_or(0);
        let offered = harnesses.clone();
        let picker = div()
            .id("agent-harness")
            .role(Role::RadioGroup)
            .aria_label("Agent")
            // One Tab stop; the arrows pick, as in a macOS radio group.
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if let Some(step) = arrow(&event.keystroke.key) {
                    let next = at.saturating_add_signed(step).min(offered.len() - 1);
                    this.pick_harness(offered[next], cx);
                    cx.stop_propagation();
                }
            }))
            .rounded(px(theme::RADIUS_ROW))
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .children(harnesses.into_iter().enumerate().map(|(ix, each)| {
                let selected = each == harness;
                div()
                    .id(("agent-harness", ix))
                    .role(Role::RadioButton)
                    .aria_label(each.name())
                    .aria_selected(selected)
                    .h(px(theme::BUTTON_HEIGHT))
                    .px_3()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(theme::TEXT_SMALL))
                    .border_1()
                    .map(|d| {
                        if selected {
                            d.bg(content(0.14))
                                .border_color(content(0.14))
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            d.border_color(content(0.06))
                                .text_color(content(0.65))
                                .cursor_pointer()
                                .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                        }
                    })
                    .child(icon(each.icon(), 14.).text_color(content(if selected {
                        1.
                    } else {
                        0.55
                    })))
                    .child(each.name())
                    .when(self.added.contains(&each), |d| d.child(added_dot()))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_harness(each, cx)))
            }));
        let failed = self
            .job
            .as_ref()
            .filter(|job| job.harness == harness)
            .and_then(|job| job.failed.clone());
        let about = match &failed {
            Some(error) => div().text_color(theme::danger()).child(error.clone()),
            None if self.added.contains(&harness) => {
                div().text_color(content(0.65)).child(harness.then())
            }
            None => div().text_color(content(0.65)).child(harness.adds()),
        };
        let by_hand = self.by_hand;
        let disclosure = div()
            .id("agent-by-hand")
            .role(Role::Button)
            .aria_label("Do it yourself")
            .aria_expanded(by_hand)
            .self_start()
            .rounded_full()
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(theme::TEXT_CAPTION))
            .text_color(content(0.65))
            .cursor_pointer()
            .hover(|s| s.text_color(content(1.)))
            .child(
                icon(
                    if by_hand {
                        "chevron-down-hi"
                    } else {
                        "chevron-right"
                    },
                    12.,
                )
                .text_color(content(0.55)),
            )
            .child("Do it yourself")
            .on_click(cx.listener(|this, _, _, cx| {
                this.by_hand = !this.by_hand;
                cx.notify();
            }));
        let setup = div()
            .py(px(12.))
            .flex()
            .flex_col()
            .gap_4()
            .child(self.focusable("agent-harness", true, picker, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_size(px(theme::TEXT_BODY)).child(harness.name()))
                            .child(
                                about
                                    .text_size(px(theme::TEXT_CAPTION))
                                    .line_height(px(16.)),
                            ),
                    )
                    .child(self.action_button(window, cx)),
            )
            .child(self.focusable("agent-by-hand", true, disclosure, cx))
            .when(by_hand, |d| {
                d.child(step_row(
                    1,
                    step,
                    Some(self.code_box("copy-setup", code, cx)),
                ))
                .child(step_row(2, harness.then().to_string(), None))
            });
        let mut mcp = vec![server.into_any_element()];
        match self.installed.as_deref() {
            Some([]) => mcp.push(
                div()
                    .py(px(12.))
                    .text_size(px(theme::TEXT_CAPTION))
                    .line_height(px(16.))
                    .text_color(content(0.65))
                    .child("No coding agents found. The command above works with any that runs MCP servers.")
                    .into_any_element(),
            ),
            Some(_) => mcp.push(setup.into_any_element()),
            None => {}
        }
        let profile = if p.agents_all_tools {
            Profile::All
        } else {
            Profile::Core
        };
        let tools = TOOLS
            .iter()
            .filter(|tool| profile.offers(tool.profile) && (p.localsend || !tool.is_localsend()))
            .map(|tool| {
                div()
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_size(px(theme::TEXT_SMALL))
                            .child(tool.name),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_CAPTION))
                            .line_height(px(16.))
                            .text_color(content(0.65))
                            .child(tool.description),
                    )
                    .into_any_element()
            })
            .collect();
        vec![
            group(
                "MCP",
                Some(
                    "Agents run Monodrive as an MCP server and search the same index as this window.",
                ),
                mcp,
            ),
            group(
                "Files",
                None,
                vec![
                    row(
                        "Let agents change files",
                        Some(
                            "Trash, put back, rename, create, copy, move, undo and redo. Off, agents can only read.",
                        ),
                        self.focusable(
                            "agents-change-files",
                            true,
                            toggle(
                                "agents-change-files",
                                "Let agents change files",
                                p.agents_change_files,
                                |p, on| p.agents_change_files = on,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Management tools",
                        Some(
                            "Also offer places, servers, saved searches, Git, saved commands, edits and organize. Each costs an agent's context; new sessions pick this up.",
                        ),
                        self.focusable(
                            "agents-all-tools",
                            true,
                            toggle(
                                "agents-all-tools",
                                "Management tools",
                                p.agents_all_tools,
                                |p, on| p.agents_all_tools = on,
                            ),
                            cx,
                        ),
                    ),
                ],
            ),
            group(
                "Tools",
                Some(
                    "What agents can call. The ones that change files need Let agents change files.",
                ),
                tools,
            ),
        ]
    }
}
