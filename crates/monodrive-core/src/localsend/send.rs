//! Sending files to a device: offer them, then upload what it accepts, one
//! file at a time. Every request is pinned to the device's certificate.

use super::{Device, Identity, PORT, runtime, unreachable};
use futures_util::StreamExt;
use localsend::http::client::ClientError;
use localsend::http::dto_v2::PrepareUploadRequestDtoV2;
use localsend::model::transfer::{FileContent, FileDto, FileMetadata};
use localsend::util::error::ErrorChain;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Stops a send, from any thread.
#[derive(Clone, Default)]
pub struct Cancel(CancellationToken);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }
}

/// How a send is going.
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    /// Offered; the receiver hasn't answered.
    Waiting,
    /// Accepted, and `sent` of `total` bytes are there.
    Sending { sent: u64, total: u64 },
}

/// What arrived.
#[derive(Clone, Debug, PartialEq)]
pub struct Sent {
    pub files: usize,
    pub bytes: u64,
    /// Files the receiver didn't take, when it took some.
    pub declined: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SendError {
    /// Nothing under the paths is a file.
    Nothing,
    NotFound(PathBuf),
    Declined,
    /// It wants a PIN, or another one.
    Pin {
        wrong: bool,
    },
    /// Too many wrong PINs; it refuses this Mac for a while.
    TooManyTries,
    /// It's receiving from someone else.
    Busy,
    Unreachable(String),
    Cancelled,
    Failed(String),
}

impl SendError {
    pub fn says(&self, to: &str) -> String {
        match self {
            Self::Nothing => "There's no file to send.".into(),
            Self::NotFound(path) => format!("{} doesn't exist.", path.display()),
            Self::Declined => format!("{to} declined."),
            Self::Pin { wrong: false } => format!("{to} asks for a PIN."),
            Self::Pin { wrong: true } => format!("{to} didn't take that PIN."),
            Self::TooManyTries => format!("{to} refuses after too many wrong PINs. Try later."),
            Self::Busy => format!("{to} is receiving from someone else."),
            Self::Unreachable(why) => format!("{to} didn't answer: {why}"),
            Self::Cancelled => "Cancelled.".into(),
            Self::Failed(why) => why.clone(),
        }
    }
}

/// Sends `paths` to `device` and waits until it's done.
pub fn send(
    identity: &Identity,
    device: &Device,
    paths: &[PathBuf],
    pin: Option<&str>,
    progress: impl Fn(Progress) + Send + Sync + 'static,
    cancel: &Cancel,
) -> Result<Sent, SendError> {
    let files = offer(paths)?;
    runtime().block_on(run(
        identity,
        device,
        files,
        pin,
        Arc::new(progress),
        cancel.0.clone(),
    ))
}

/// What a send started with `spawn_send` says.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Progress(Progress),
    Done(Result<Sent, SendError>),
}

/// `send` on LocalSend's thread. `update` hears progress, then once how it
/// ended.
pub fn spawn_send(
    identity: Arc<Identity>,
    device: Device,
    paths: Vec<PathBuf>,
    pin: Option<String>,
    cancel: Cancel,
    update: impl Fn(Update) + Send + Sync + 'static,
) {
    let update = Arc::new(update);
    runtime().spawn(async move {
        let tell = update.clone();
        let result = match offer(&paths) {
            Ok(files) => {
                run(
                    &identity,
                    &device,
                    files,
                    pin.as_deref(),
                    Arc::new(move |progress| tell(Update::Progress(progress))),
                    cancel.0,
                )
                .await
            }
            Err(e) => Err(e),
        };
        update(Update::Done(result));
    });
}

/// A file to send, and where it is.
struct Offered {
    dto: FileDto,
    path: PathBuf,
}

/// Each file under `paths`, a folder's named by its path from the folder,
/// as LocalSend's app and CLI send them. Links and special files stay.
fn offer(paths: &[PathBuf]) -> Result<Vec<Offered>, SendError> {
    let mut files = Vec::new();
    for path in paths {
        let meta = path
            .symlink_metadata()
            .map_err(|_| SendError::NotFound(path.clone()))?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| SendError::NotFound(path.clone()))?;
        walk(path, &meta, name, &mut files)
            .map_err(|e| SendError::Failed(format!("{}: {e}", path.display())))?;
    }
    if files.is_empty() {
        return Err(SendError::Nothing);
    }
    Ok(files)
}

fn walk(
    path: &Path,
    meta: &std::fs::Metadata,
    name: String,
    files: &mut Vec<Offered>,
) -> std::io::Result<()> {
    if meta.is_file() {
        if name == ".DS_Store" || name.ends_with("/.DS_Store") {
            return Ok(());
        }
        files.push(Offered {
            dto: FileDto {
                id: files.len().to_string(),
                file_type: mime(&name).into(),
                file_name: name,
                size: meta.len(),
                sha256: None,
                preview: None,
                metadata: FileMetadata::from_fs_metadata(meta),
            },
            path: path.to_path_buf(),
        });
    } else if meta.is_dir() {
        let mut children: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let inner = child.path();
            let meta = inner.symlink_metadata()?;
            let child_name = format!("{name}/{}", child.file_name().to_string_lossy());
            walk(&inner, &meta, child_name, files)?;
        }
    }
    Ok(())
}

/// The MIME type LocalSend's apps go by, such as Android saving pictures to
/// the gallery.
fn mime(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "m4v" => "video/x-m4v",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "apk" => "application/vnd.android.package-archive",
        "txt" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

async fn run(
    identity: &Identity,
    device: &Device,
    files: Vec<Offered>,
    pin: Option<&str>,
    progress: Arc<dyn Fn(Progress) + Send + Sync>,
    cancel: CancellationToken,
) -> Result<Sent, SendError> {
    let client = identity
        .client(device.pin(), None)
        .map_err(SendError::Failed)?;
    let protocol = device.protocol();
    let offered = files.len();
    let mut files: HashMap<String, Offered> = files
        .into_iter()
        .map(|file| (file.dto.id.clone(), file))
        .collect();
    let payload = PrepareUploadRequestDtoV2 {
        info: identity.register(super::receiving_on().unwrap_or(PORT)),
        files: files
            .iter()
            .map(|(id, file)| (id.clone(), file.dto.clone()))
            .collect(),
    };
    progress(Progress::Waiting);
    let prepared = client
        .prepare_upload(
            protocol,
            &device.host,
            device.port,
            None,
            payload,
            pin,
            cancel.clone(),
        )
        .await
        .map_err(|e| refused(e, pin.is_some()))?;
    let Some(session) = prepared.response else {
        return Err(SendError::Declined);
    };
    let mut accepted: Vec<(Offered, String)> = session
        .files
        .into_iter()
        .filter_map(|(id, token)| Some((files.remove(&id)?, token)))
        .collect();
    if accepted.is_empty() {
        return Err(SendError::Declined);
    }
    accepted.sort_by(|(a, _), (b, _)| a.dto.file_name.cmp(&b.dto.file_name));
    let total: u64 = accepted.iter().map(|(file, _)| file.dto.size).sum();
    let mut sent = 0;
    progress(Progress::Sending { sent, total });
    let count = accepted.len();
    for (file, token) in accepted {
        let before = sent;
        let tell = progress.clone();
        let uploaded = client
            .upload(
                protocol,
                &device.host,
                device.port,
                None,
                &session.session_id,
                &file.dto.id,
                &token,
                body(file.path.clone(), move |bytes| {
                    tell(Progress::Sending {
                        sent: before + bytes,
                        total,
                    })
                }),
                cancel.clone(),
            )
            .await;
        if let Err(e) = uploaded {
            let _ = client
                .cancel(protocol, &device.host, device.port, &session.session_id)
                .await;
            return Err(match e {
                ClientError::Cancelled => SendError::Cancelled,
                e => SendError::Failed(format!(
                    "Couldn't send {}: {}",
                    file.dto.file_name,
                    ErrorChain(&e)
                )),
            });
        }
        sent += file.dto.size;
    }
    Ok(Sent {
        files: count,
        bytes: sent,
        declined: offered - count,
    })
}

/// A file streamed as an upload's body, saying how many bytes went.
fn body(path: PathBuf, sent: impl Fn(u64) + Send + 'static) -> localsend::reqwest::Body {
    let mut total = 0u64;
    localsend::reqwest::Body::wrap_stream(FileContent::Path(path).into_stream().map(move |chunk| {
        let chunk = chunk?;
        total += chunk.len() as u64;
        sent(total);
        Ok::<_, std::io::Error>(chunk)
    }))
}

/// Why `prepare-upload` didn't start a transfer, by LocalSend's statuses.
fn refused(e: ClientError, gave_pin: bool) -> SendError {
    match e {
        ClientError::Cancelled => SendError::Cancelled,
        ClientError::StatusCode(status) => match status.status {
            401 => SendError::Pin { wrong: gave_pin },
            403 => SendError::Declined,
            409 => SendError::Busy,
            429 => SendError::TooManyTries,
            code => SendError::Failed(match status.message {
                Some(message) => format!("It answered {code}: {message}"),
                None => format!("It answered {code}."),
            }),
        },
        e if e.is_unreachable() => SendError::Unreachable(unreachable(ErrorChain(&e).to_string())),
        e => SendError::Failed(ErrorChain(&e).to_string()),
    }
}
