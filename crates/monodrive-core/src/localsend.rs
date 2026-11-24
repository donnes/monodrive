//! LocalSend: finding devices nearby, sending them files, and receiving
//! theirs, through LocalSend's own protocol core (v2.2). The window and the
//! MCP server both call this. `.plans/31-localsend.md` and
//! `docs/localsend.md` say how it fits together.

mod messages;
mod nearby;
mod receive;
mod send;

#[cfg(test)]
mod tests;

pub use messages::{KEPT, Message, Messages};
pub use nearby::{Nearby, look, merged};
pub use receive::{Ended, Event, Offer, Receiver};
pub use send::{Cancel, Outgoing, Progress, SendError, Sent, Update, send, spawn_send};

use localsend::discovery::DeviceIdentity;
use localsend::http::client::LsHttpClientV2;
use localsend::http::dto_v2::RegisterDtoV2;
use localsend::http::server::TlsConfig;
use localsend::http::state::ClientInfo;
use localsend::model::discovery::{DeviceType, PROTOCOL_VERSION_V2, ProtocolType};
use localsend::multicast::MulticastDevice;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

/// The port LocalSend listens and announces on.
pub const PORT: u16 = localsend::multicast::DEFAULT_PORT;

/// What other devices show under this Mac's name.
const MODEL: &str = "Monodrive";

/// This Mac as LocalSend knows it: a self-signed certificate whose SHA-256
/// fingerprint identifies it. The window and every MCP server share one, so
/// peers see one device.
pub struct Identity {
    /// The Mac's computer name.
    pub alias: String,
    pub fingerprint: String,
    cert_pem: String,
    key_pem: String,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    cert_pem: String,
    key_pem: String,
    fingerprint: String,
}

impl Identity {
    /// The identity in the support folder, made there on first use.
    pub fn user() -> Result<Identity, String> {
        let dir = crate::support_dir()
            .ok_or("No home folder.")?
            .join("localsend");
        Self::load(&dir, computer_name())
    }

    /// `identity.json` in `dir`, made with a new key when there's none.
    pub fn load(dir: &Path, alias: String) -> Result<Identity, String> {
        let path = dir.join("identity.json");
        let saved = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Saved>(&bytes)
                .map_err(|e| format!("{} doesn't read: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let made = localsend::crypto::cert::generate_self_signed()
                    .map_err(|e| format!("Couldn't make LocalSend's certificate: {e:#}"))?;
                let saved = Saved {
                    cert_pem: made.certificate_pem,
                    key_pem: made.private_key_pem,
                    fingerprint: made.fingerprint,
                };
                let bytes = serde_json::to_vec(&saved).map_err(|e| e.to_string())?;
                match publish(&path, &bytes) {
                    Ok(()) => saved,
                    // The window and an agent made one at once; the first to
                    // land is this Mac's.
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Self::load(dir, alias);
                    }
                    Err(e) => return Err(format!("Couldn't save {}: {e}", path.display())),
                }
            }
            Err(e) => return Err(format!("Couldn't read {}: {e}", path.display())),
        };
        Ok(Identity {
            alias,
            fingerprint: saved.fingerprint,
            cert_pem: saved.cert_pem,
            key_pem: saved.key_pem,
        })
    }

    /// A client that only talks to the certificate `pinned`, or to anyone
    /// with `None`, which only finding devices may use.
    fn client(
        &self,
        pinned: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<LsHttpClientV2, String> {
        LsHttpClientV2::try_new(
            &self.key_pem,
            &self.cert_pem,
            pinned.map(str::to_owned),
            timeout,
        )
        .map_err(|e| format!("Couldn't set up LocalSend's connection: {e}"))
    }

    fn tls(&self) -> TlsConfig {
        TlsConfig {
            cert: self.cert_pem.clone(),
            private_key: self.key_pem.clone(),
        }
    }

    fn device_identity(&self) -> DeviceIdentity {
        DeviceIdentity {
            cert_pem: self.cert_pem.clone(),
            private_key_pem: self.key_pem.clone(),
        }
    }

    fn info(&self) -> ClientInfo {
        ClientInfo {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION_V2.into(),
            device_model: Some(MODEL.into()),
            device_type: Some(DeviceType::Desktop),
            token: self.fingerprint.clone(),
        }
    }

    /// Who's asking, with the port this Mac receives on, or LocalSend's
    /// own while it isn't receiving.
    fn register(&self, port: u16) -> RegisterDtoV2 {
        RegisterDtoV2 {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION_V2.into(),
            device_model: Some(MODEL.into()),
            device_type: Some(DeviceType::Desktop),
            fingerprint: self.fingerprint.clone(),
            port,
            protocol: ProtocolType::Https,
            download: false,
        }
    }

    fn multicast(&self, port: u16) -> MulticastDevice {
        MulticastDevice {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION_V2.into(),
            device_model: Some(MODEL.into()),
            device_type: Some(DeviceType::Desktop),
            fingerprint: self.fingerprint.clone(),
            port,
            protocol: ProtocolType::Https,
            download: false,
        }
    }
}

/// Writes `bytes` beside `path` and moves them there whole, never over
/// another identity.
fn publish(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let staging = path.with_extension(format!("json.{}", std::process::id()));
    write_private(&staging, bytes)?;
    crate::ops::disk::rename_new(&staging, path).inspect_err(|_| {
        #[expect(
            clippy::disallowed_methods,
            reason = "our own copy of the key, which never took the identity's name"
        )]
        let _ = std::fs::remove_file(&staging);
    })
}

/// The key never leaves this account.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The name Sharing settings give this Mac, as AirDrop shows it.
pub fn computer_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

/// A device that answered, as LocalSend describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Device {
    pub alias: String,
    /// The SHA-256 of its certificate, which every request to it is pinned to.
    pub fingerprint: String,
    pub kind: Kind,
    /// Such as "iPhone", "Samsung" or "Windows", when it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// An IP address, `fe80::1%4` for a link-local one.
    pub host: String,
    pub port: u16,
    /// LocalSend's encryption, on unless the device turned it off.
    pub https: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Phone,
    Computer,
    Browser,
    Headless,
    Server,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Phone => "phone",
            Kind::Computer => "computer",
            Kind::Browser => "browser",
            Kind::Headless => "headless",
            Kind::Server => "server",
        }
    }

    fn of(kind: Option<&DeviceType>) -> Kind {
        match kind {
            Some(DeviceType::Mobile) => Kind::Phone,
            Some(DeviceType::Web) => Kind::Browser,
            Some(DeviceType::Headless) => Kind::Headless,
            Some(DeviceType::Server) => Kind::Server,
            Some(DeviceType::Desktop) | None => Kind::Computer,
        }
    }
}

impl Device {
    fn protocol(&self) -> ProtocolType {
        if self.https {
            ProtocolType::Https
        } else {
            ProtocolType::Http
        }
    }

    /// The fingerprint to pin to: over plain HTTP there's no certificate.
    fn pin(&self) -> Option<&str> {
        self.https.then_some(self.fingerprint.as_str())
    }
}

/// What a peer says about itself, cut to one short line: it reaches the
/// window and agents as it is.
fn tidy(text: &str) -> String {
    let line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = line.trim();
    match line.char_indices().nth(64) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

/// The port this process receives on, 0 while it isn't.
static RECEIVING_ON: AtomicU16 = AtomicU16::new(0);

/// Where this Mac takes files, which a send tells the receiver so it can
/// list this Mac.
fn receiving_on() -> Option<u16> {
    match RECEIVING_ON.load(Ordering::Relaxed) {
        0 => None,
        port => Some(port),
    }
}

/// LocalSend's core runs on tokio; GPUI's executor and the MCP server's
/// thread don't, so this process keeps one runtime for it.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("localsend")
            .enable_all()
            .build()
            .expect("a tokio runtime starts")
    })
}

/// What's useful to say when a peer didn't answer. macOS answers "No route
/// to host" when Local Network access is off for the process, which for an
/// agent's server is whichever app started the agent.
fn unreachable(why: String) -> String {
    let blocked = why.contains("No route to host") || why.contains("os error 65");
    if blocked {
        format!(
            "{why}. macOS may be blocking Local Network access for this app; for an agent, \
             that's the app that started it (System Settings > Privacy & Security > Local Network)."
        )
    } else {
        why
    }
}
