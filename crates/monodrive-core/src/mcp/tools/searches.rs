//! The sidebar's saved searches: list_searches, save_search, rename_search
//! and remove_search. None of them runs a search.

use super::{Call, local_one};
use crate::mcp::contract::{Reply, ToolError, shown};
use crate::mcp::server::Server;
use crate::places::SavedSearch;
use crate::search::Scope;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, JsonSchema)]
pub struct Search {
    pub name: String,
    /// In find_files syntax.
    pub query: String,
    /// The folder it searches, when it searches one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Search {
    fn of(search: &SavedSearch) -> Self {
        Self {
            name: search.name.clone(),
            query: search.query.clone(),
            path: match &search.scope {
                Scope::Everywhere => None,
                Scope::Folder(folder) => Some(shown(folder)),
            },
        }
    }

    /// `name: query`, and the folder it searches when it has one.
    fn line(&self) -> String {
        match &self.path {
            None => format!("{}: {}\n", self.name, self.query),
            Some(folder) => format!("{}: {} (in {folder})\n", self.name, self.query),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSearches {}

#[derive(Serialize, JsonSchema)]
pub struct Searches {
    pub searches: Vec<Search>,
}

impl Call for ListSearches {
    type Out = Searches;

    fn run(self, server: &mut Server) -> Result<Reply<Searches>, ToolError> {
        let searches: Vec<Search> = server.places.searches()?.iter().map(Search::of).collect();
        let text: String = if searches.is_empty() {
            "No saved searches.\n".into()
        } else {
            searches.iter().map(Search::line).collect()
        };
        Ok(Reply::new(text, Searches { searches }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveSearch {
    name: String,
    query: String,
    /// A folder to search in.
    path: Option<String>,
    /// Its position; 0 is first.
    at: Option<usize>,
}

impl Call for SaveSearch {
    type Out = Search;

    fn run(self, server: &mut Server) -> Result<Reply<Search>, ToolError> {
        let scope = match &self.path {
            Some(path) => Scope::Folder(local_one(path, "save_search")?),
            None => Scope::Everywhere,
        };
        let search = SavedSearch {
            name: self.name,
            query: self.query,
            scope,
        };
        let saved = Search::of(&server.places.save_search(search, self.at)?);
        Ok(Reply::new(saved.line(), saved))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameSearch {
    name: String,
    to: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Named {
    pub name: String,
}

impl Call for RenameSearch {
    type Out = Named;

    fn run(self, server: &mut Server) -> Result<Reply<Named>, ToolError> {
        server.places.rename_search(&self.name, &self.to)?;
        let to = self.to.trim().to_owned();
        Ok(Reply::new(
            format!("Renamed {} to {to}.\n", self.name),
            Named { name: to },
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveSearch {
    name: String,
}

impl Call for RemoveSearch {
    type Out = Named;

    fn run(self, server: &mut Server) -> Result<Reply<Named>, ToolError> {
        server.places.remove_search(&self.name)?;
        Ok(Reply::new(
            format!("Removed the saved search {}.\n", self.name),
            Named { name: self.name },
        ))
    }
}
