//! The sidebar's places and saved servers: list_places, pin, unpin, eject,
//! connect, disconnect, add_server and forget_server. Servers are smb://
//! shares, which mount, or SSH servers, browsed as `id:/path`.

use super::{Call, local_one};
use crate::cloud;
use crate::expand_home;
use crate::mcp::contract::{Code, Reply, ToolError, shown};
use crate::mcp::server::Server;
use crate::places::Availability;
use crate::remote::Source;
use crate::servers::{self, Prompt};
use crate::volumes::{self, VolumeId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListPlaces {}

#[derive(Serialize, JsonSchema)]
pub struct Favorite {
    pub path: String,
    /// Why it can't be opened: its volume isn't connected, or it's gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Volume {
    pub path: String,
    pub name: String,
    pub free: u64,
    pub total: u64,
    pub startup: bool,
    pub read_only: bool,
    pub network: bool,
    pub removable: bool,
    /// Its UUID, or a share's address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct CloudFolder {
    pub path: String,
    pub name: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Saved {
    /// An smb:// address, or an SSH server's id.
    pub name: String,
    /// smb or ssh.
    pub kind: String,
    /// An SSH server's host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Where a share is mounted, or where an SSH server's browsing starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Mounted, or connected in this session.
    pub connected: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct Places {
    pub favorites: Vec<Favorite>,
    pub volumes: Vec<Volume>,
    pub cloud: Vec<CloudFolder>,
    pub servers: Vec<Saved>,
}

impl Call for ListPlaces {
    type Out = Places;

    fn run(self, server: &mut Server) -> Result<Reply<Places>, ToolError> {
        let mounted = volumes::list();
        let favorites: Vec<Favorite> = server
            .places
            .places(&mounted)?
            .iter()
            .map(|place| match &place.availability {
                Availability::Available(path) => Favorite {
                    path: shown(path),
                    unavailable: None,
                },
                Availability::Unavailable(why) => Favorite {
                    path: shown(&place.favorite.path),
                    unavailable: Some(why.clone()),
                },
            })
            .collect();
        let volumes: Vec<Volume> = mounted
            .iter()
            .map(|volume| Volume {
                path: shown(&volume.mount),
                name: volume.name.clone(),
                free: volume.free,
                total: volume.total,
                startup: volume.startup,
                read_only: volume.read_only,
                network: volume.network,
                removable: volume.ejectable || volume.removable,
                id: volume.id.as_ref().map(|id| match id {
                    VolumeId::Uuid(uuid) => uuid.clone(),
                    VolumeId::Url(url) => url.clone(),
                }),
            })
            .collect();
        let cloud: Vec<CloudFolder> = cloud::folders(&expand_home("~"))
            .iter()
            .map(|folder| CloudFolder {
                path: shown(&folder.path),
                name: folder.name.clone(),
            })
            .collect();
        let mut servers: Vec<Saved> = server
            .places
            .servers()?
            .into_iter()
            .map(|address| {
                let at = servers::mounted(&address, &mounted).map(|volume| shown(&volume.mount));
                Saved {
                    connected: at.is_some(),
                    name: address,
                    kind: "smb".into(),
                    host: None,
                    at,
                }
            })
            .collect();
        servers.extend(
            server
                .places
                .remotes()?
                .into_iter()
                .map(|source| ssh(server, source)),
        );
        let text = places_text(&favorites, &volumes, &cloud, &servers);
        Ok(Reply::new(
            text,
            Places {
                favorites,
                volumes,
                cloud,
                servers,
            },
        ))
    }
}

fn ssh(server: &Server, source: Source) -> Saved {
    Saved {
        connected: server.remote.is_connected(&source.id),
        at: source.start,
        host: Some(source.host),
        name: source.id,
        kind: "ssh".into(),
    }
}

/// Finder's decimal units, to a size worth reading for a disk.
fn bytes(n: u64) -> String {
    let n = n as f64;
    match n {
        ..1e9 => format!("{:.0} MB", n / 1e6),
        ..1e12 => format!("{:.1} GB", n / 1e9),
        _ => format!("{:.2} TB", n / 1e12),
    }
}

/// Each section's paths or names, a line each, with what's worth knowing
/// in brackets.
fn places_text(
    favorites: &[Favorite],
    volumes: &[Volume],
    cloud: &[CloudFolder],
    servers: &[Saved],
) -> String {
    let mut out = String::from("Favorites\n");
    for favorite in favorites {
        let _ = match &favorite.unavailable {
            None => writeln!(out, "{}", favorite.path),
            Some(why) => writeln!(out, "{} (unavailable: {why})", favorite.path),
        };
    }
    out.push_str("Volumes\n");
    for volume in volumes {
        let mut about = vec![
            volume.name.clone(),
            format!("{} free of {}", bytes(volume.free), bytes(volume.total)),
        ];
        for (yes, label) in [
            (volume.startup, "startup disk"),
            (volume.read_only, "read-only"),
            (volume.network, "network"),
            (volume.removable, "removable"),
        ] {
            if yes {
                about.push(label.into());
            }
        }
        if let Some(id) = &volume.id {
            about.push(id.clone());
        }
        let _ = writeln!(out, "{} ({})", volume.path, about.join(", "));
    }
    out.push_str("Cloud\n");
    for folder in cloud {
        let _ = writeln!(out, "{} ({})", folder.path, folder.name);
    }
    out.push_str("Servers\n");
    for server in servers {
        let _ = match (&server.host, &server.at) {
            (Some(host), start) => writeln!(
                out,
                "{}: (ssh {host}, starts at {}, {})",
                server.name,
                start.as_deref().unwrap_or("its default folder"),
                if server.connected {
                    "connected"
                } else {
                    "not connected"
                }
            ),
            (None, Some(at)) => writeln!(out, "{} (connected at {at})", server.name),
            (None, None) => writeln!(out, "{}", server.name),
        };
    }
    out
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    path: String,
    /// Its position; 0 is first.
    at: Option<usize>,
}

/// The place or server a tool acted on.
#[derive(Serialize, JsonSchema)]
pub struct Done {
    pub name: String,
}

impl Call for Pin {
    type Out = Done;

    fn run(self, server: &mut Server) -> Result<Reply<Done>, ToolError> {
        let path = local_one(&self.path, "pin")?;
        let pinned = server.places.pin(&path, self.at, &volumes::list())?;
        Ok(Reply::new(
            format!("{}\n", pinned.display()),
            Done {
                name: shown(&pinned),
            },
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Unpin {
    path: String,
}

impl Call for Unpin {
    type Out = Done;

    fn run(self, server: &mut Server) -> Result<Reply<Done>, ToolError> {
        let path = local_one(&self.path, "unpin")?;
        server.places.unpin(&path, &volumes::list())?;
        Ok(Reply::new(
            format!("Unpinned {}; the folder is untouched.\n", path.display()),
            Done { name: shown(&path) },
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Eject {
    /// Where the volume is mounted.
    path: String,
}

impl Call for Eject {
    type Out = Done;

    fn run(self, server: &mut Server) -> Result<Reply<Done>, ToolError> {
        let path = local_one(&self.path, "eject")?;
        let origin = server.origin();
        let done = volumes::eject(&path, &origin, &mut server.ops)?;
        Ok(Reply::new(done + "\n", Done { name: shown(&path) }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Connect {
    /// smb://host/share or sftp://host/folder, with no password.
    address: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Connected {
    /// The smb:// address, or the SSH server's id.
    pub name: String,
    /// Where a share is mounted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
}

impl Call for Connect {
    type Out = Connected;

    fn run(self, server: &mut Server) -> Result<Reply<Connected>, ToolError> {
        let typed = servers::server(&self.address)?;
        if !server.ops.agents_change_files() {
            return Err(ToolError::new(
                Code::AgentsDisabled,
                "Agents can't connect to servers: Let agents change files is off in Monodrive's Agents settings.",
            ));
        }
        match typed {
            servers::Server::Smb(address) => {
                let cancel = Arc::default();
                let (address, at) =
                    servers::connect(&server.places, &address, &cancel, |address| {
                        servers::mount(address, Prompt::Never, &cancel)
                    })?;
                Ok(Reply::new(
                    format!("{address} is at {}\n", at.display()),
                    Connected {
                        name: address,
                        mount: Some(shown(&at)),
                    },
                ))
            }
            servers::Server::Sftp { host, start } => {
                let source = server.remote.add(&host, start.as_deref(), || false)?;
                let starts = source.start.as_deref().unwrap_or("its default folder");
                Ok(Reply::new(
                    format!(
                        "{}: {}, starts at {starts}; browse it as {}:/path\n",
                        source.id, source.host, source.id
                    ),
                    Connected {
                        name: source.id,
                        mount: None,
                    },
                ))
            }
        }
    }
}

/// Whether `name` is a saved SSH server's id rather than an smb:// address.
fn is_ssh(server: &Server, name: &str) -> Result<bool, ToolError> {
    Ok(server
        .places
        .remotes()?
        .iter()
        .any(|source| source.id == name))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Disconnect {
    /// An smb:// address, or an SSH server's id.
    server: String,
}

impl Call for Disconnect {
    type Out = Done;

    fn run(self, server: &mut Server) -> Result<Reply<Done>, ToolError> {
        if is_ssh(server, &self.server)? {
            let text = if server.remote.disconnect(&self.server) {
                format!(
                    "Disconnected from {}. Its jobs on the owner keep their own connection.\n",
                    self.server
                )
            } else {
                format!("{} wasn't connected.\n", self.server)
            };
            return Ok(Reply::new(text, Done { name: self.server }));
        }
        let address = servers::address(&self.server)?;
        let origin = server.origin();
        let volumes = volumes::list();
        let volume = servers::mounted(&address, &volumes)
            .ok_or_else(|| ToolError::new(Code::NotFound, format!("{address} isn't connected.")))?;
        let done = volumes::eject(&volume.mount, &origin, &mut server.ops)?;
        Ok(Reply::new(done + "\n", Done { name: address }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddServer {
    /// A host, user@host or an alias in the user's ssh config.
    host: String,
    /// What its paths start with; made from the host when left out.
    id: Option<String>,
    /// Where browsing starts; the server's default folder when left out.
    path: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Added {
    pub id: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
}

impl Call for AddServer {
    type Out = Added;

    fn run(self, server: &mut Server) -> Result<Reply<Added>, ToolError> {
        server.can_change()?;
        let source =
            server
                .places
                .add_remote(&self.host, self.id.as_deref(), self.path.as_deref())?;
        let start = source
            .start
            .as_deref()
            .unwrap_or("the server's default folder");
        let text = format!(
            "{}: {}, starts at {start}; browse it as {}:/path\n",
            source.id, source.host, source.id
        );
        Ok(Reply::new(
            text,
            Added {
                id: source.id,
                host: source.host,
                start: source.start,
            },
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgetServer {
    /// An smb:// address, or an SSH server's id.
    server: String,
}

impl Call for ForgetServer {
    type Out = Done;

    fn run(self, server: &mut Server) -> Result<Reply<Done>, ToolError> {
        if is_ssh(server, &self.server)? {
            let source = server.places.forget_remote(&self.server)?;
            server.remote.disconnect(&source.id);
            return Ok(Reply::new(
                format!("Forgot {}; nothing on the server changed.\n", source.id),
                Done { name: source.id },
            ));
        }
        let address = servers::address(&self.server)?;
        server.places.forget_server(&address)?;
        Ok(Reply::new(
            format!("Forgot {address}; a share it mounted stays mounted.\n"),
            Done { name: address },
        ))
    }
}
