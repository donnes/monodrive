//! check_updates: what the updater would offer, without installing it.

use super::Call;
use crate::mcp::contract::{Reply, ToolError};
use crate::mcp::server::Server;
use crate::updates::{self, Channel};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckUpdates {
    channel: Option<Channel>,
}

#[derive(Serialize, JsonSchema)]
pub struct Release {
    pub version: String,
    /// Its page on GitHub.
    pub page: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Releases {
    /// This Monodrive's version.
    pub current: String,
    /// Newer releases, newest first.
    pub releases: Vec<Release>,
}

impl Call for CheckUpdates {
    type Out = Releases;

    fn run(self, _: &mut Server) -> Result<Reply<Releases>, ToolError> {
        let pending = updates::pending(self.channel.unwrap_or_else(updates::saved_channel))?;
        let releases: Vec<Release> = pending
            .iter()
            .map(|release| Release {
                version: release.version.to_string(),
                page: release.page.to_string(),
            })
            .collect();
        let text = if releases.is_empty() {
            format!("Monodrive {} is up to date.\n", crate::VERSION)
        } else {
            releases
                .iter()
                .map(|release| format!("{} {}\n", release.version, release.page))
                .collect()
        };
        Ok(Reply::new(
            text,
            Releases {
                current: crate::VERSION.into(),
                releases,
            },
        ))
    }
}
