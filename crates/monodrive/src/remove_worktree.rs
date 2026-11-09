//! Removing a worktree's question, in the palette's card: what goes to the
//! Trash, and anything only that worktree holds, or why Monodrive won't.
//! Esc, Cancel or a click outside keeps it; ↩ does what the last button says.

use crate::palette;
use crate::theme::{self, ButtonLook, content, text_button};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, Window, actions,
    div, prelude::*, px,
};
use monodrive_core::ops::Request;
use monodrive_core::worktrees::Removal;
use std::path::{Path, PathBuf};

actions!(remove_worktree, [Cancel, Confirm]);

pub fn bind(cx: &mut App) {
    let context = Some("RemoveWorktree");
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, context),
        KeyBinding::new("enter", Confirm, context),
    ]);
}

/// What the person is asked.
#[derive(Clone, Debug)]
pub enum Ask {
    /// Git allows it. `repo` is the repository's common Git folder.
    Remove { repo: PathBuf, removal: Box<Removal> },
    /// Why it stays, in Monodrive's or Git's words.
    Refused { path: PathBuf, why: String },
}

pub enum Answer {
    Cancel,
    /// What the owner is asked, carrying the risk shown, so it refuses if
    /// there's more by then.
    Remove(Request),
}

pub struct RemoveWorktreeView {
    ask: Ask,
    focus: FocusHandle,
}

impl EventEmitter<Answer> for RemoveWorktreeView {}

impl Focusable for RemoveWorktreeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The card's title, body lines and code boxes, and its last button.
struct Said {
    title: String,
    lines: Vec<Part>,
    button: (&'static str, ButtonLook),
}

enum Part {
    Text(String),
    /// Paths or commits, in a code box.
    Listed(Vec<String>),
}

impl RemoveWorktreeView {
    pub fn new(ask: Ask, cx: &mut Context<Self>) -> Self {
        Self {
            ask,
            focus: cx.focus_handle(),
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(match &self.ask {
            Ask::Remove { repo, removal } => Answer::Remove(Request::RemoveWorktree {
                repo: repo.clone(),
                path: removal.worktree.path.clone(),
                confirmed: removal.risk.clone(),
            }),
            Ask::Refused { .. } => Answer::Cancel,
        });
    }

    fn said(&self) -> Said {
        let removal = match &self.ask {
            Ask::Refused { path, why } => {
                // The title names it, so the reason does by name too.
                let why = why.replace(&path.display().to_string(), &quoted(path));
                return Said {
                    title: format!("{} stays", quoted(path)),
                    lines: vec![Part::Text(why)],
                    button: ("OK", ButtonLook::Primary),
                };
            }
            Ask::Remove { removal, .. } => removal,
        };
        let worktree = &removal.worktree;
        let name = quoted(&worktree.path);
        let branch_stays = worktree
            .branch
            .as_ref()
            .map(|branch| format!("The branch {branch} stays."));
        let risk = &removal.risk;
        let missing = removal.folder.is_none();
        let gone = "Its folder isn't where Git expects it, maybe on a drive that isn't connected, so only Git's registration of it goes to the Trash.";
        if missing && risk.is_empty() {
            return Said {
                title: format!("Forget {name}?"),
                lines: vec![Part::Text(format!("{gone} ⌘Z puts it back."))],
                button: ("Forget", ButtonLook::Primary),
            };
        }
        if risk.is_empty() {
            let mut text = "The folder goes to the Trash and Git forgets the worktree.".to_string();
            if let Some(stays) = &branch_stays {
                text.push(' ');
                text.push_str(stays);
            }
            text.push_str(" ⌘Z puts both back.");
            return Said {
                title: format!("Move {name} to the Trash?"),
                lines: vec![Part::Text(text)],
                button: ("Move to Trash", ButtonLook::Primary),
            };
        }
        let mut lines = Vec::new();
        if missing {
            lines.push(Part::Text(gone.into()));
        }
        if risk.paths > 0 {
            lines.push(Part::Text(format!(
                "{} go to the Trash with it:",
                counted(risk.paths, "uncommitted change")
            )));
            lines.push(Part::Listed(more(&risk.files, risk.paths)));
        }
        if risk.unpushed > 0 {
            let commits = counted(risk.unpushed, "commit");
            lines.push(Part::Text(match &worktree.branch {
                Some(branch) => {
                    format!("{commits} on {branch} that no other branch, tag or remote has:")
                }
                None => {
                    let them = if risk.unpushed == 1 { "it" } else { "them" };
                    format!(
                        "{commits} on no branch, tag or remote. Git may drop {them} at its next cleanup, and ⌘Z can't bring {them} back after that:"
                    )
                }
            }));
            let commits: Vec<String> = risk.commits.iter().map(ToString::to_string).collect();
            lines.push(Part::Listed(more(&commits, risk.unpushed)));
        }
        let mut last = branch_stays.unwrap_or_default();
        if !last.is_empty() {
            last.push(' ');
        }
        last.push_str("⌘Z puts it all back.");
        lines.push(Part::Text(last));
        Said {
            title: format!("{name} holds work nothing else does"),
            lines,
            button: if missing {
                ("Forget Anyway", ButtonLook::Caution)
            } else {
                ("Move to Trash Anyway", ButtonLook::Caution)
            },
        }
    }
}

/// "1 commit", "3 uncommitted changes".
fn counted(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// What a code box lists, and how many more there are.
fn more(shown: &[String], all: usize) -> Vec<String> {
    let mut lines = shown.to_vec();
    if all > shown.len() {
        lines.push(format!("and {} more", all - shown.len()));
    }
    lines
}

fn quoted(path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    format!("\"{name}\"")
}

impl Render for RemoveWorktreeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let said = self.said();
        let refused = matches!(self.ask, Ask::Refused { .. });
        let body = div()
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .gap_3()
            .children(said.lines.into_iter().map(|part| {
                match part {
                    Part::Text(text) => div()
                        .text_size(px(theme::TEXT_BODY))
                        .text_color(content(0.65))
                        .child(text),
                    Part::Listed(lines) => div()
                        .px_3()
                        .py_2()
                        .rounded(px(theme::RADIUS_CARD))
                        .bg(content(0.03))
                        .border_1()
                        .border_color(content(0.06))
                        .font_family(theme::MONO_FONT)
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .flex()
                        .flex_col()
                        .children(lines.into_iter().map(|line| div().truncate().child(line))),
                }
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .when(!refused, |d| {
                        d.child(
                            text_button("remove-worktree-cancel", "Cancel", ButtonLook::Secondary)
                                .debug_selector(|| "remove-worktree-cancel".into())
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(Answer::Cancel))),
                        )
                    })
                    .child(
                        text_button("remove-worktree-confirm", said.button.0, said.button.1)
                            .debug_selector(|| "remove-worktree-confirm".into())
                            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                    ),
            );
        palette::overlay("remove-worktree-overlay")
            .key_context("RemoveWorktree")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(Answer::Cancel)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(Answer::Cancel)),
            )
            .child(
                palette::card("remove-worktree", "Remove Worktree", said.title, body).w(px(440.)),
            )
    }
}
