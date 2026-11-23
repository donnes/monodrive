//! The text devices sent this Mac, newest first, which the LocalSend page
//! lists to copy again or forget. Only the window writes it. A message may
//! hold what a person wouldn't save anywhere, so the file is this account's
//! alone.

use super::write_private;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

/// How many are kept.
pub const KEPT: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// When it arrived, in milliseconds since 1970, which also names it.
    pub at: u64,
    pub from: String,
    pub text: String,
}

pub struct Messages {
    path: PathBuf,
}

impl Messages {
    /// `localsend/messages.json` in the support folder.
    pub fn user() -> Option<Messages> {
        Some(Messages::at(
            crate::support_dir()?.join("localsend/messages.json"),
        ))
    }

    pub fn at(path: PathBuf) -> Messages {
        Messages { path }
    }

    pub fn list(&self) -> Result<Vec<Message>, String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{} doesn't read: {e}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("Couldn't read {}: {e}", self.path.display())),
        }
    }

    /// Keeps `text` from `from` first, and the newest `KEPT` in all.
    pub fn add(&self, from: String, text: String, at: u64) -> Result<Vec<Message>, String> {
        self.change(|messages| {
            let at = messages.first().map_or(at, |newest| at.max(newest.at + 1));
            messages.insert(0, Message { at, from, text });
            messages.truncate(KEPT);
        })
    }

    pub fn forget(&self, at: u64) -> Result<Vec<Message>, String> {
        self.change(|messages| messages.retain(|message| message.at != at))
    }

    pub fn clear(&self) -> Result<Vec<Message>, String> {
        self.change(Vec::clear)
    }

    /// One change at a time, so two close together don't lose one.
    fn change(&self, change: impl FnOnce(&mut Vec<Message>)) -> Result<Vec<Message>, String> {
        static CHANGING: Mutex<()> = Mutex::new(());
        let _one = CHANGING.lock().unwrap_or_else(PoisonError::into_inner);
        let mut messages = self.list()?;
        change(&mut messages);
        let fail = |e: std::io::Error| format!("Couldn't save {}: {e}", self.path.display());
        let json = serde_json::to_vec(&messages).map_err(|e| e.to_string())?;
        let staging = self
            .path
            .with_extension(format!("json.{}", std::process::id()));
        write_private(&staging, &json).map_err(fail)?;
        std::fs::rename(&staging, &self.path).map_err(fail)?;
        Ok(messages)
    }
}
