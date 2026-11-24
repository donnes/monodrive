//! localsend_devices and localsend_send: devices nearby running LocalSend,
//! and sending them files or text. Agents don't receive: a person answers each
//! incoming transfer in the window.

use super::{Call, local};
use crate::localsend::{self, Cancel, Device, Outgoing, SendError};
use crate::mcp::contract::{Code, Reply, ToolError};
use crate::mcp::session::Session;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const fn default_wait() -> u64 {
    3
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalsendDevices {
    /// How long to look, 1 to 10 seconds.
    #[serde(default = "default_wait")]
    wait_seconds: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Devices {
    pub devices: Vec<Device>,
}

impl Call for LocalsendDevices {
    type Out = Devices;

    fn run(self, session: &mut Session) -> Result<Reply<Devices>, ToolError> {
        let identity = session.localsend()?;
        let wait = Duration::from_secs(self.wait_seconds.clamp(1, 10));
        let devices = localsend::look(identity, wait);
        session.heard(&devices);
        let text = if devices.is_empty() {
            "No LocalSend device answered. A device needs LocalSend open on this network. If \
             one has, macOS may be blocking Local Network access for the app that started this \
             agent (System Settings > Privacy & Security > Local Network).\n"
                .to_string()
        } else {
            devices
                .iter()
                .map(|device| {
                    let model = device
                        .model
                        .as_ref()
                        .map(|model| format!(", {model}"))
                        .unwrap_or_default();
                    format!(
                        "{} ({}{model}) {}\n",
                        device.alias,
                        device.kind.name(),
                        device.fingerprint
                    )
                })
                .collect()
        };
        Ok(Reply::new(text, Devices { devices }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalsendSend {
    /// A device's fingerprint, as localsend_devices listed it in this session.
    fingerprint: String,
    /// Files or folders on this Mac.
    paths: Option<Vec<String>>,
    /// Text to send instead of paths, which the receiver shows as a message.
    text: Option<String>,
    /// The PIN the receiver asks for, if it asks.
    pin: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Sent {
    /// The device's name.
    pub to: String,
    pub files: usize,
    pub bytes: u64,
    /// Files it didn't take, when it took some.
    pub declined: usize,
}

impl Call for LocalsendSend {
    type Out = Sent;

    fn run(self, session: &mut Session) -> Result<Reply<Sent>, ToolError> {
        let device = session.nearby(&self.fingerprint).ok_or_else(|| {
            ToolError::new(
                Code::NotFound,
                format!(
                    "No device with fingerprint {} was listed in this session. \
                         localsend_devices lists them.",
                    self.fingerprint
                ),
            )
        })?;
        let outgoing = match (self.paths, self.text) {
            (Some(paths), None) => Outgoing::Files(local(&paths, "localsend_send")?),
            (None, Some(text)) => Outgoing::Text(text),
            _ => {
                return Err(ToolError::new(
                    Code::InvalidArgument,
                    "Pass either paths or text.",
                ));
            }
        };
        let identity = session.localsend()?;
        let sent = localsend::send(
            &identity,
            &device,
            &outgoing,
            self.pin.as_deref(),
            |_| {},
            &Cancel::new(),
        )
        .map_err(|e| {
            let code = match &e {
                SendError::Nothing => Code::InvalidArgument,
                SendError::NotFound(_) => Code::NotFound,
                SendError::Declined => Code::PermissionDenied,
                SendError::Pin { .. } => Code::AuthRequired,
                SendError::Busy | SendError::TooManyTries => Code::Busy,
                SendError::Unreachable(_) | SendError::NoAnswer => Code::Unreachable,
                SendError::Cancelled | SendError::Failed(_) => Code::Failed,
            };
            let error = ToolError::new(code, e.says(&device.alias));
            match e {
                SendError::NotFound(path) => error.at(path.display().to_string()),
                _ => error,
            }
        })?;
        let declined = match sent.declined {
            0 => String::new(),
            n => format!(", {n} declined"),
        };
        let text = match outgoing {
            Outgoing::Text(_) => format!("Sent the text to {}.\n", device.alias),
            Outgoing::Files(_) | Outgoing::Bytes { .. } => format!(
                "Sent {} file{} ({} bytes) to {}{declined}.\n",
                sent.files,
                if sent.files == 1 { "" } else { "s" },
                sent.bytes,
                device.alias
            ),
        };
        Ok(Reply::new(
            text,
            Sent {
                to: device.alias,
                files: sent.files,
                bytes: sent.bytes,
                declined: sent.declined,
            },
        ))
    }
}
