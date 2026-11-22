//! Anonymous usage events, sent to PostHog only when the user opted in. An
//! event carries a random ID made for this Mac, the app's version and macOS's,
//! and never a name, path or anything else about the user's files. PostHog
//! makes no person profile and skips its location lookup for them.

use serde::Serialize;
use std::process::Command;

/// PostHog's project key, set only by the release workflow. Dev builds and
/// builds without it send nothing. The workflow sets an unset variable empty.
const KEY: Option<&str> = match option_env!("MONODRIVE_POSTHOG_KEY") {
    Some(key) if !crate::DEV && !key.is_empty() => Some(key),
    _ => None,
};

const HOST: &str = match option_env!("MONODRIVE_POSTHOG_HOST") {
    Some(host) if !host.is_empty() => host,
    _ => "https://us.i.posthog.com",
};

/// Whether this build can send events at all.
pub const AVAILABLE: bool = KEY.is_some();

/// A new random ID for this Mac, as 32 hex digits.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    // SAFETY: arc4random_buf fills exactly `bytes.len()` bytes of a live buffer.
    unsafe { libc::arc4random_buf(bytes.as_mut_ptr().cast(), bytes.len()) };
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Serialize)]
struct Capture<'a> {
    api_key: &'a str,
    event: &'a str,
    distinct_id: &'a str,
    properties: Properties,
}

#[derive(Serialize)]
struct Properties {
    #[serde(rename = "$process_person_profile")]
    person_profile: bool,
    #[serde(rename = "$geoip_disable")]
    geoip_disable: bool,
    #[serde(rename = "$app_version")]
    app_version: &'static str,
    #[serde(rename = "$os")]
    os: &'static str,
    #[serde(rename = "$os_version")]
    os_version: String,
}

fn body(key: &str, id: &str, event: &str) -> Result<String, String> {
    let version = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
    serde_json::to_string(&Capture {
        api_key: key,
        event,
        distinct_id: id,
        properties: Properties {
            person_profile: false,
            geoip_disable: true,
            app_version: crate::VERSION,
            os: "macOS",
            os_version: format!(
                "{}.{}.{}",
                version.majorVersion, version.minorVersion, version.patchVersion
            ),
        },
    })
    .map_err(|e| e.to_string())
}

/// Sends `event` as the Mac with `id`. Blocks on the network, so call it off
/// the UI thread.
pub fn send(id: &str, event: &str) -> Result<(), String> {
    let Some(key) = KEY else { return Ok(()) };
    let body = body(key, id, event)?;
    let output = Command::new("/usr/bin/curl")
        .args([
            // Ignores ~/.curlrc, which could add headers or send it elsewhere.
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--max-time",
            "10",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
        ])
        .arg(body)
        .arg(format!("{HOST}/i/v0/e/"))
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_names_no_person_and_carries_only_versions() {
        let body = body("key", "abc", "app_opened").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
        let properties = sent["properties"].as_object().unwrap();
        let mut keys: Vec<&str> = properties.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "$app_version",
                "$geoip_disable",
                "$os",
                "$os_version",
                "$process_person_profile"
            ]
        );
        assert_eq!(properties["$process_person_profile"], false);
        assert_eq!(properties["$geoip_disable"], true);
        assert_eq!(sent["distinct_id"], "abc");
    }
}
