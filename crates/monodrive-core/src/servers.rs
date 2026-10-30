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
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Whether macOS may ask for credentials on screen. An MCP server has nobody
/// to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt {
    Allow,
    Never,
}

/// The address to mount for what was typed: `smb://` is assumed, a password
/// is refused and a trailing `/` goes. Refusals never repeat what was typed,
/// which may hold a password this didn't recognize as one.
pub fn address(typed: &str) -> Result<String, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("Type a server address, such as smb://nas.local/media.".into());
    }
    let rest = match typed.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("smb") => rest,
        Some(_) => return Err("Monodrive connects to smb:// servers.".into()),
        None => typed,
    };
    let (authority, share) = rest.split_once('/').unwrap_or((rest, ""));
    let (user, host) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(user, host)| (Some(user), host));
    let hides_colon = |user: &str| user.to_ascii_lowercase().contains("%3a");
    if user.is_some_and(|user| user.contains(':') || hides_colon(user)) {
        return Err(
            "Leave the password out of the address. macOS asks for it and keeps it in the Keychain."
                .into(),
        );
    }
    if host.is_empty() {
        return Err("That address doesn't name a server.".into());
    }
    // A port is digits, so `me:pass/word@nas` isn't a host and a port.
    let bad_port = !host.starts_with('[')
        && host
            .rsplit_once(':')
            .is_some_and(|(_, port)| !port.chars().all(|c| c.is_ascii_digit()));
    if bad_port
        || (authority.contains(':') && share.contains('@'))
        || authority.chars().any(char::is_whitespace)
        || typed.contains(['?', '#'])
        || typed.chars().any(char::is_control)
    {
        return Err("That isn't a server address.".into());
    }
    let share = share.trim_end_matches('/').replace(' ', "%20");
    Ok(if share.is_empty() {
        format!("smb://{authority}")
    } else {
        format!("smb://{authority}/{share}")
    })
}

/// `url` without any password, as macOS's remount address could hold one
/// for a share someone else mounted.
pub fn without_password(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.into();
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    match authority.rsplit_once('@') {
        Some((user, host)) => {
            let user = user.split(':').next().unwrap_or_default();
            format!("{scheme}://{user}@{host}{path}")
        }
        None => url.into(),
    }
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
/// connected, and whose attempt nobody cancelled, is kept. Returns it and
/// where it's mounted.
pub fn connect(
    store: &Store,
    typed: &str,
    cancel: &Cancel,
    mount: impl FnOnce(&str) -> Result<PathBuf, String>,
) -> Result<(String, PathBuf), String> {
    let address = address(typed)?;
    let at = mount(&address)?;
    if cancel.is_cancelled() {
        return Err(CANCELLED.into());
    }
    store.save_server(&address)?;
    Ok((address, at))
}

const CANCELLED: &str = "Cancelled.";

/// Stops a mount in flight, along with any prompt it has up.
#[derive(Default)]
pub struct Cancel(Mutex<Attempt>);

#[derive(Default)]
struct Attempt {
    cancelled: bool,
    /// NetFS's id while the mount is in flight.
    request: Option<usize>,
    /// Whoever waits for the mount, until it has ended.
    waiting: Option<Sender<Ended>>,
}

/// How a mount ended. NetFS never reports a cancelled one, so whichever of
/// its report and a cancel comes first ends it, and the other does nothing.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Cancelled,
    Reported { status: i32, at: Option<PathBuf> },
}

impl Attempt {
    fn end(&mut self, ended: Ended) {
        if let Some(waiting) = self.waiting.take() {
            let _ = waiting.send(ended);
        }
    }
}

impl Cancel {
    fn attempt(&self) -> MutexGuard<'_, Attempt> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the attempt at once, then asks NetFS to drop it on another
    /// thread, so this never waits on macOS and is safe on the UI thread.
    pub fn cancel(self: &Arc<Self>) {
        let mut attempt = self.attempt();
        attempt.cancelled = true;
        attempt.end(Ended::Cancelled);
        if attempt.request.is_none() {
            return;
        }
        drop(attempt);
        let this = self.clone();
        std::thread::spawn(move || {
            let mut attempt = this.attempt();
            if let Some(request) = attempt.request.take() {
                // SAFETY: NetFS's report clears the id under this lock, so
                // it's still in flight here.
                unsafe { NetFSMountURLCancel(request as *mut c_void) };
            }
        });
    }

    pub fn is_cancelled(&self) -> bool {
        self.attempt().cancelled
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
    let (waiting, done) = mpsc::channel();
    let finished = {
        let cancel = cancel.clone();
        RcBlock::new(
            move |status: i32, _: *mut c_void, mounts: *const NSArray<NSString>| {
                // SAFETY: NetFS passes an array of paths, or null on failure.
                let first = unsafe { mounts.as_ref() }.and_then(NSArray::firstObject);
                let at = first.map(|path| PathBuf::from(path.to_string()));
                // Holding the lock waits for `start` to have noted the id.
                let mut attempt = cancel.attempt();
                attempt.request = None;
                attempt.end(Ended::Reported { status, at });
            },
        )
    };
    start(address, prompt, cancel, waiting, &finished)?;
    let (status, at) = match done.recv() {
        Ok(Ended::Reported { status, at }) => (status, at),
        Ok(Ended::Cancelled) => return Err(CANCELLED.into()),
        Err(_) => return Err(format!("{address}: macOS didn't answer.")),
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
    waiting: Sender<Ended>,
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
        let mut attempt = cancel.attempt();
        if attempt.cancelled {
            return Err(CANCELLED.into());
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
        attempt.waiting = Some(waiting);
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
            "{address} needs credentials, and macOS can only ask for them on screen. Connect once from Monodrive's window (⇧⌘K) and keep them in the Keychain."
        );
    }
    let why = match status {
        USER_CANCELLED | libc::ECANCELED => return CANCELLED.into(),
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
        assert!(refused("afp://nas/media").starts_with("Monodrive connects to smb://"));
        assert!(refused("smb:///media").contains("doesn't name a server"));
        assert!(refused("smb://nas local/media").contains("isn't a server address"));
        assert!(refused("smb://nas/media?x=1").contains("isn't a server address"));
        // However a password is written, no refusal repeats it.
        for hidden in [
            "smb://me:secret@nas/media",
            "me:secret@nas/media",
            "smb://me%3Asecret@nas/media",
            "smb://me:secret/word@nas/media",
            "smb://me:123/secret@nas/media",
            "afp://me:secret@nas/media",
            "me:secret://nas/media",
            "smb://me:secret@/media",
            "smb://me:secret@nas local/media",
        ] {
            let why = refused(hidden);
            assert!(!why.contains("secret"), "{hidden}: {why}");
        }
    }

    #[test]
    fn a_remount_address_loses_any_password_and_nothing_else() {
        assert_eq!(
            without_password("smb://me:secret@nas.local/media/"),
            "smb://me@nas.local/media/"
        );
        assert_eq!(
            without_password("smb://me@nas.local/media"),
            "smb://me@nas.local/media"
        );
        assert_eq!(without_password("smb://nas/a@b"), "smb://nas/a@b");
    }

    /// An attempt in flight, as `start` leaves it, and its waiter.
    fn in_flight() -> (Arc<Cancel>, mpsc::Receiver<Ended>) {
        let cancel = Arc::new(Cancel::default());
        let (waiting, done) = mpsc::channel();
        cancel.attempt().waiting = Some(waiting);
        (cancel, done)
    }

    #[test]
    fn a_cancel_ends_the_wait_since_netfs_never_reports_a_cancelled_mount() {
        let (cancel, done) = in_flight();
        cancel.cancel();
        assert_eq!(done.try_recv(), Ok(Ended::Cancelled));
        assert!(cancel.is_cancelled());
        // A report arriving anyway finds it over, and a second cancel too.
        cancel.attempt().end(Ended::Reported {
            status: 0,
            at: None,
        });
        cancel.cancel();
        assert!(done.try_recv().is_err());
    }

    #[test]
    fn a_report_ends_the_wait_and_a_later_cancel_changes_nothing() {
        let (cancel, done) = in_flight();
        let report = || Ended::Reported {
            status: 0,
            at: Some("/Volumes/media".into()),
        };
        cancel.attempt().end(report());
        cancel.cancel();
        assert_eq!(done.try_recv(), Ok(report()));
        assert!(done.try_recv().is_err());
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
        let refused = connect(&store, "nas.local/media", &Cancel::default(), |address| {
            assert_eq!(address, "smb://nas.local/media");
            Err("smb://nas.local/media: no.".into())
        });
        assert_eq!(refused, Err("smb://nas.local/media: no.".into()));
        assert_eq!(store.servers(), Ok(Vec::new()));
        // A password in the address never reaches the mount or the file.
        let refused = connect(
            &store,
            "smb://me:secret@nas/media",
            &Cancel::default(),
            |_| unreachable!("refused before mounting"),
        );
        assert!(refused.is_err());
        assert!(!store.places.exists());
        // Cancelled while it mounted: the mount may have won, the save doesn't.
        let cancel = Arc::new(Cancel::default());
        let cancelled = connect(&store, "nas.local/media", &cancel, |_| {
            cancel.cancel();
            Ok("/Volumes/media".into())
        });
        assert_eq!(cancelled, Err("Cancelled.".into()));
        assert_eq!(store.servers(), Ok(Vec::new()));
        let connected = connect(&store, "nas.local/media", &Cancel::default(), |_| {
            Ok("/Volumes/media".into())
        });
        assert_eq!(
            connected,
            Ok(("smb://nas.local/media".into(), "/Volumes/media".into()))
        );
        connect(&store, "smb://nas.local/media/", &Cancel::default(), |_| {
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
