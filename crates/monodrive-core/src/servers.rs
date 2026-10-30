//! Connect to Server: what an address may be, and mounting a share through
//! macOS's NetFS, which asks for credentials itself and keeps them in the
//! Keychain. Monodrive never sees a password. Connecting only mounts; listing
//! the share is a separate step.

use crate::places::Store;
use crate::volumes::{self, Volume, VolumeId};
use block2::{Block, RcBlock};
use objc2_foundation::{NSArray, NSMutableDictionary, NSString, NSURL};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

/// Whether macOS may ask for credentials on screen. An MCP server has nobody
/// to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt {
    Allow,
    Never,
}

/// The address to mount for what was typed: `smb://` is assumed, a password
/// is refused and a trailing `/` goes.
pub fn address(typed: &str) -> Result<String, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("Type a server address, such as smb://nas.local/media.".into());
    }
    let rest = match typed.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("smb") => rest,
        Some((scheme, _)) => {
            return Err(format!(
                "Monodrive connects to smb:// servers, not {scheme}://."
            ));
        }
        None => typed,
    };
    let (authority, share) = rest.split_once('/').unwrap_or((rest, ""));
    let (user, host) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(user, host)| (Some(user), host));
    if user.is_some_and(|user| user.contains(':')) {
        return Err(
            "Leave the password out of the address. macOS asks for it and keeps it in the Keychain."
                .into(),
        );
    }
    if host.is_empty() {
        return Err(format!("{typed} doesn't name a server."));
    }
    if authority.chars().any(char::is_whitespace) || typed.chars().any(char::is_control) {
        return Err(format!("{typed} isn't a server address."));
    }
    let share = share.trim_end_matches('/').replace(' ', "%20");
    Ok(if share.is_empty() {
        format!("smb://{authority}")
    } else {
        format!("smb://{authority}/{share}")
    })
}

/// The mounted volume that's the share at `address`. macOS's remount address
/// may add the account, and shares and hosts ignore case.
pub fn mounted<'a>(address: &str, volumes: &'a [Volume]) -> Option<&'a Volume> {
    let plain = |address: &str| {
        let rest = address.strip_prefix("smb://").unwrap_or(address);
        let (authority, share) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        format!("{host}/{}", share.trim_end_matches('/')).to_lowercase()
    };
    let wanted = plain(address);
    volumes
        .iter()
        .find(|volume| matches!(&volume.id, Some(VolumeId::Url(url)) if plain(url) == wanted))
}

/// Mounts with `mount`, then saves the address: only an address that
/// connected is kept. Returns it and where it's mounted.
pub fn connect(
    store: &Store,
    typed: &str,
    mount: impl FnOnce(&str) -> Result<PathBuf, String>,
) -> Result<(String, PathBuf), String> {
    let address = address(typed)?;
    let at = mount(&address)?;
    store.save_server(&address)?;
    Ok((address, at))
}

/// Stops a mount in flight, along with any prompt it has up.
#[derive(Default)]
pub struct Cancel(Mutex<Attempt>);

#[derive(Default)]
struct Attempt {
    cancelled: bool,
    /// NetFS's id while the mount is in flight.
    request: Option<usize>,
}

impl Cancel {
    pub fn cancel(&self) {
        let mut attempt = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        attempt.cancelled = true;
        if let Some(request) = attempt.request {
            // SAFETY: the lock keeps the id from finishing during the call.
            unsafe { NetFSMountURLCancel(request as *mut c_void) };
        }
    }
}

#[link(name = "NetFS", kind = "framework")]
unsafe extern "C" {
    fn NetFSMountURLAsync(
        url: *const c_void,
        mountpath: *const c_void,
        user: *const c_void,
        passwd: *const c_void,
        open_options: *const c_void,
        mount_options: *const c_void,
        request: *mut *mut c_void,
        dispatchq: *mut c_void,
        report: &Block<dyn Fn(i32, *mut c_void, *const NSArray<NSString>)>,
    ) -> i32;
    fn NetFSMountURLCancel(request: *mut c_void) -> i32;
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
}

/// The share at `address` where it's mounted, mounting it if it isn't yet.
/// Waits for macOS, and for the person at any prompt, so call it off the UI
/// thread.
pub fn mount(address: &str, prompt: Prompt, cancel: &Arc<Cancel>) -> Result<PathBuf, String> {
    if let Some(volume) = mounted(address, &volumes::list()) {
        return Ok(volume.mount.clone());
    }
    let (sent, done) = std::sync::mpsc::channel();
    let finished = {
        let cancel = cancel.clone();
        RcBlock::new(
            move |status: i32, _: *mut c_void, mounts: *const NSArray<NSString>| {
                // Holding the lock waits for `start` to have noted the id.
                let mut attempt = cancel.0.lock().unwrap_or_else(PoisonError::into_inner);
                attempt.request = None;
                drop(attempt);
                // SAFETY: NetFS passes an array of paths, or null on failure.
                let first = unsafe { mounts.as_ref() }.and_then(NSArray::firstObject);
                let _ = sent.send((status, first.map(|path| PathBuf::from(path.to_string()))));
            },
        )
    };
    start(address, prompt, cancel, &finished)?;
    let Ok((status, at)) = done.recv() else {
        return Err(format!("{address}: macOS didn't answer."));
    };
    match (status, at) {
        (0, Some(at)) => Ok(at),
        // Under an address other than this one, such as a Bonjour name.
        (libc::EEXIST, _) => Err(format!("{address} is already connected.")),
        (0, None) => Err(format!("{address}: macOS didn't say where it mounted.")),
        (status, _) => Err(failure(address, status, prompt)),
    }
}

/// Asks NetFS to mount, with no user or password so that it finds them or
/// asks for them itself. Returns once it's on its way.
fn start(
    address: &str,
    prompt: Prompt,
    cancel: &Cancel,
    finished: &Block<dyn Fn(i32, *mut c_void, *const NSArray<NSString>)>,
) -> Result<(), String> {
    objc2::rc::autoreleasepool(|_| {
        let Some(url) = NSURL::URLWithString(&NSString::from_str(address)) else {
            return Err(format!("{address} isn't a server address."));
        };
        let open = NSMutableDictionary::<NSString, NSString>::new();
        let ui = match prompt {
            Prompt::Allow => "AllowUI",
            Prompt::Never => "NoUI",
        };
        // SAFETY: both are valid strings, and the dictionary copies the key.
        unsafe {
            open.setObject_forKey(
                &NSString::from_str(ui),
                objc2::runtime::ProtocolObject::from_ref(&*NSString::from_str("UIOption")),
            );
        }
        let mut attempt = cancel.0.lock().unwrap_or_else(PoisonError::into_inner);
        if attempt.cancelled {
            return Err("Cancelled.".into());
        }
        let mut request = std::ptr::null_mut();
        let url: *const NSURL = &*url;
        let open: *const NSMutableDictionary<NSString, NSString> = &*open;
        let none = std::ptr::null();
        // SAFETY: NSURL and NSMutableDictionary are toll-free bridged to the
        // CF types NetFS takes; NetFS copies the block, and null leaves the
        // mount point, user, password and mount options to macOS.
        let status = unsafe {
            NetFSMountURLAsync(
                url.cast(),
                none,
                none,
                none,
                open.cast(),
                none,
                &mut request,
                dispatch_get_global_queue(0, 0),
                finished,
            )
        };
        if status != 0 {
            return Err(failure(address, status, prompt));
        }
        attempt.request = Some(request as usize);
        Ok(())
    })
}

const USER_CANCELLED: i32 = -128;
const PASSWORD_NEEDS_CHANGE: i32 = -5045;
const ACCOUNT_RESTRICTED: i32 = -5999;
const NO_SHARES: i32 = -5998;
const NO_AUTH_METHOD: i32 = -5997;
const NO_PROTOCOL_VERSION: i32 = -5996;

/// What NetFS's status means, for the person or agent.
fn failure(address: &str, status: i32, prompt: Prompt) -> String {
    let needs_someone = matches!(
        status,
        libc::EAUTH | libc::EACCES | libc::EPERM | USER_CANCELLED | PASSWORD_NEEDS_CHANGE
    );
    if prompt == Prompt::Never && needs_someone {
        return format!(
            "{address} needs credentials, and macOS can only ask for them on screen. Connect once from Monodrive's window (⌘K) and keep them in the Keychain."
        );
    }
    let why = match status {
        USER_CANCELLED | libc::ECANCELED => return "Cancelled.".into(),
        libc::EAUTH | libc::EACCES | libc::EPERM => {
            "the server refused these credentials, or this account has no permission.".to_string()
        }
        PASSWORD_NEEDS_CHANGE => "the account's password needs changing.".into(),
        ACCOUNT_RESTRICTED => "the account is locked or restricted right now.".into(),
        NO_SHARES => "no shares are available to this account.".into(),
        NO_AUTH_METHOD => "the server offers no sign-in method macOS can use.".into(),
        NO_PROTOCOL_VERSION => "the server offers no protocol version macOS can use.".into(),
        libc::ENOENT => "there's no such server or share.".into(),
        libc::ETIMEDOUT
        | libc::EHOSTDOWN
        | libc::EHOSTUNREACH
        | libc::ENETUNREACH
        | libc::ECONNREFUSED => {
            format!(
                "the server didn't answer ({}).",
                std::io::Error::from_raw_os_error(status)
            )
        }
        status if status > 0 => std::io::Error::from_raw_os_error(status).to_string(),
        status => format!("NetFS error {status}."),
    };
    format!("{address}: {why}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_assumes_smb_keeps_the_account_and_refuses_a_password() {
        let ok = |typed: &str| address(typed).unwrap();
        assert_eq!(ok(" nas.local/media/ "), "smb://nas.local/media");
        assert_eq!(ok("SMB://nas.local"), "smb://nas.local");
        assert_eq!(
            ok("smb://donald@nas:445/My Share"),
            "smb://donald@nas:445/My%20Share"
        );
        let refused = |typed: &str| address(typed).unwrap_err();
        assert!(refused("").starts_with("Type a server address"));
        assert!(refused("smb://donald:hunter2@nas/media").starts_with("Leave the password out"));
        assert!(refused("afp://nas/media").contains("not afp://"));
        assert!(refused("smb:///media").contains("doesn't name a server"));
        assert!(refused("smb://nas local/media").contains("isn't a server address"));
    }

    fn share(url: &str) -> Volume {
        Volume {
            name: "media".into(),
            mount: "/Volumes/media".into(),
            disk: None,
            id: Some(VolumeId::Url(url.into())),
            total: 0,
            free: 0,
            read_only: false,
            network: true,
            ejectable: false,
            removable: false,
            startup: false,
        }
    }

    #[test]
    fn a_share_is_found_whatever_account_or_case_macos_reports_it_under() {
        let volumes = [share("smb://Donald@NAS.local/Media/")];
        assert!(mounted("smb://nas.local/media", &volumes).is_some());
        assert!(mounted("smb://nas.local/photos", &volumes).is_none());
        assert!(mounted("smb://other.local/media", &volumes).is_none());
    }

    #[test]
    fn only_an_address_that_connected_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            places: dir.path().join("places.json"),
            recents: dir.path().join("recents.json"),
        };
        let refused = connect(&store, "nas.local/media", |address| {
            assert_eq!(address, "smb://nas.local/media");
            Err("smb://nas.local/media: no.".into())
        });
        assert_eq!(refused, Err("smb://nas.local/media: no.".into()));
        assert_eq!(store.servers(), Ok(Vec::new()));
        // A password in the address never reaches the mount or the file.
        let refused = connect(&store, "smb://me:secret@nas/media", |_| {
            unreachable!("refused before mounting")
        });
        assert!(refused.is_err());
        assert!(!store.places.exists());
        let connected = connect(&store, "nas.local/media", |_| Ok("/Volumes/media".into()));
        assert_eq!(
            connected,
            Ok(("smb://nas.local/media".into(), "/Volumes/media".into()))
        );
        connect(&store, "smb://nas.local/media/", |_| {
            Ok("/Volumes/media".into())
        })
        .unwrap();
        assert_eq!(store.servers(), Ok(vec!["smb://nas.local/media".into()]));
    }

    #[test]
    fn a_failure_says_why_and_an_agent_is_told_a_prompt_is_needed() {
        let address = "smb://nas/media";
        let said = |status| failure(address, status, Prompt::Allow);
        assert!(said(libc::EAUTH).contains("refused these credentials"));
        assert!(said(libc::ETIMEDOUT).contains("didn't answer"));
        assert_eq!(said(USER_CANCELLED), "Cancelled.");
        let agent = |status| failure(address, status, Prompt::Never);
        for needs_someone in [libc::EAUTH, USER_CANCELLED] {
            assert!(agent(needs_someone).contains("only ask for them on screen"));
        }
        assert!(agent(libc::ETIMEDOUT).contains("didn't answer"));
    }
}
