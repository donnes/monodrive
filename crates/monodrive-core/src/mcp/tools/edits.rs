//! Editing and sorting files: edit_image, edit_video, organize and
//! suggest_names.

use super::{Call, Preview, local, local_one};
use crate::edits;
use crate::mcp::contract::{Change, Planned, Reply, ToolError, shown};
use crate::mcp::server::Server;
use crate::naming;
use crate::ops::{self, Request, organize};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditImage {
    path: String,
    steps: Vec<edits::Step>,
    #[serde(default)]
    replace: bool,
    request_id: Option<String>,
}

impl Call for EditImage {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        let path = local_one(&self.path, "edit_image")?;
        let extension = edits::picture_extension(&path);
        let steps = self.steps;
        save_edit(
            server,
            &path,
            &extension,
            self.replace,
            self.request_id.as_deref(),
            |source, into| edits::flatten(source, &steps, into).map(|_| ()),
        )
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditVideo {
    path: String,
    steps: edits::VideoRecipe,
    #[serde(default)]
    replace: bool,
    request_id: Option<String>,
}

impl Call for EditVideo {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        let path = local_one(&self.path, "edit_video")?;
        let extension = edits::video_extension(&path);
        let steps = self.steps;
        save_edit(
            server,
            &path,
            &extension,
            self.replace,
            self.request_id.as_deref(),
            |source, into| edits::export(source, &steps, into, &|| false),
        )
    }
}

/// Writes the edit under a hidden name, then has the owner put it in place.
/// Refused first if agents may not change files, so nothing's written.
fn save_edit(
    server: &mut Server,
    source: &Path,
    extension: &str,
    replace: bool,
    request_id: Option<&str>,
    write: impl FnOnce(&Path, &Path) -> Result<(), String>,
) -> Result<Reply<Change>, ToolError> {
    server.can_change()?;
    let opened = ops::Opened::read(source)?;
    let folder = source
        .parent()
        .ok_or_else(|| ToolError::invalid("That file has no folder."))?;
    let prepared = edits::prepared_path(folder, extension);
    let saved = write(source, &prepared)
        .map_err(ToolError::from)
        .and_then(|()| {
            let save = if replace {
                ops::SaveTo::Replace
            } else {
                ops::SaveTo::Copy
            };
            server.change(
                Request::SaveEdit {
                    source: opened,
                    prepared: prepared.clone(),
                    save,
                },
                request_id,
            )
        });
    // Already moved when it worked, so this only clears a failed one.
    edits::discard(&prepared);
    saved
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Organize {
    /// The folder to sort.
    path: Option<String>,
    #[serde(default)]
    dry_run: bool,
    /// A dry run's token: runs exactly its plan.
    confirm: Option<String>,
    request_id: Option<String>,
}

/// Items going into one new or existing folder.
#[derive(Serialize, JsonSchema)]
pub struct Sorted {
    pub folder: String,
    pub reason: String,
    pub items: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct OrganizePlan {
    pub groups: Vec<Sorted>,
    /// Files no rule placed, which stay.
    pub unplaced: usize,
}

impl Call for Organize {
    type Out = Planned<OrganizePlan>;

    fn run(self, server: &mut Server) -> Result<Reply<Self::Out>, ToolError> {
        if let Some(token) = &self.confirm {
            let Preview::Organize(groups) = server.kept(token)? else {
                return Err(ToolError::invalid("That token is another tool's plan."));
            };
            // Kept if the owner refuses it, so it can be confirmed again.
            let done = server.change(Request::Organize { groups }, self.request_id.as_deref())?;
            server.spent(token);
            return Ok(done.ran());
        }
        let (Some(path), true) = (&self.path, self.dry_run) else {
            return Err(ToolError::invalid(
                "Dry run first with path and dry_run: true, then confirm its token.",
            ));
        };
        let folder = local_one(path, "organize")?;
        let proposal = organize::propose(&folder)?;
        let mut text = String::new();
        for group in &proposal.groups {
            let _ = writeln!(text, "{}/ ({})", group.folder.display(), group.reason);
            for item in &group.items {
                let _ = writeln!(text, "  {}", item.path.display());
            }
        }
        let plan = OrganizePlan {
            groups: proposal
                .groups
                .iter()
                .map(|group| Sorted {
                    folder: shown(&group.folder),
                    reason: group.reason.clone(),
                    items: group.items.iter().map(|item| shown(&item.path)).collect(),
                })
                .collect(),
            unplaced: proposal.unplaced,
        };
        if proposal.groups.is_empty() {
            let _ = writeln!(text, "No rule places anything in {}.", folder.display());
            return Ok(Reply::new(text, plan).planned(None));
        }
        let _ = writeln!(text, "left where they are: {}", proposal.unplaced);
        let token = server.keep(Preview::Organize(proposal.groups));
        let _ = writeln!(text, "token: {token}");
        Ok(Reply::new(text, plan).planned(Some(token)))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestNames {
    paths: Vec<String>,
    #[serde(default)]
    style: naming::Style,
}

#[derive(Serialize, JsonSchema)]
pub struct Suggested {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Why there's no suggestion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Suggestions {
    pub items: Vec<Suggested>,
}

impl Call for SuggestNames {
    type Out = Suggestions;

    fn run(self, _: &mut Server) -> Result<Reply<Suggestions>, ToolError> {
        let mut text = String::new();
        let items = local(&self.paths, "suggest_names")?
            .iter()
            .map(|path| match naming::suggest(path, self.style) {
                Ok(name) => {
                    let _ = writeln!(text, "{} → {name}", path.display());
                    Suggested {
                        path: shown(path),
                        name: Some(name),
                        why: None,
                    }
                }
                Err(why) => {
                    let _ = writeln!(text, "{} (no suggestion: {why})", path.display());
                    Suggested {
                        path: shown(path),
                        name: None,
                        why: Some(why),
                    }
                }
            })
            .collect();
        Ok(Reply::new(text, Suggestions { items }))
    }
}
