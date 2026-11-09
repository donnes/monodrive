//! What a run was measured under, recorded so a number can be judged later.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conditions {
    pub commit: String,
    /// "release" or "debug", of the tool that ran the suite.
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub os: String,
    pub model: String,
    pub cores: usize,
    /// The OS's build: macOS's, or the Linux kernel's release.
    pub macos_build: String,
    pub volume_format: String,
    /// FileVault on macOS; on Linux, the block devices under the volume,
    /// where `crypt` means LUKS.
    pub file_vault: String,
    pub power: String,
    pub thermal: String,
    #[serde(default)]
    pub spotlight: String,
    pub load_before: f64,
    pub load_after: f64,
}

fn output(program: &str, args: &[&str]) -> String {
    Command::new(program).args(args).output().map_or_else(
        |error| format!("unknown: {error}"),
        |out| String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

pub fn load() -> f64 {
    let mut average = [0f64; 1];
    // SAFETY: a buffer of one and a count of one.
    let read = unsafe { libc::getloadavg(average.as_mut_ptr(), 1) };
    if read == 1 { average[0] } else { f64::NAN }
}

/// Load past this share of the cores makes a run noisy.
const BUSY_SHARE: f64 = 0.25;

impl Conditions {
    /// The conditions now, for work on the volume `dir` is on.
    pub fn now(dir: &Path) -> Self {
        let dir_text = dir.to_string_lossy();
        let mut conditions = Self {
            commit: output("git", &["rev-parse", "--short", "HEAD"]),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_string(),
            hostname: output("hostname", &["-s"]),
            os: std::env::consts::OS.to_string(),
            model: String::new(),
            cores: std::thread::available_parallelism().map_or(0, usize::from),
            macos_build: String::new(),
            volume_format: String::new(),
            file_vault: String::new(),
            power: String::new(),
            thermal: String::new(),
            spotlight: String::new(),
            load_before: load(),
            load_after: f64::NAN,
        };
        if cfg!(target_os = "macos") {
            let mount = output("df", &[&dir_text]);
            let device = mount
                .lines()
                .nth(1)
                .and_then(|line| line.split_whitespace().next())
                .unwrap_or("")
                .to_string();
            conditions.volume_format = output("mount", &[])
                .lines()
                .find(|line| line.starts_with(&format!("{device} on ")))
                .and_then(|line| line.rsplit_once(" (")?.1.split(',').next())
                .unwrap_or("unknown")
                .to_string();
            conditions.model = output("sysctl", &["-n", "hw.model"]);
            conditions.macos_build = output("sw_vers", &["-buildVersion"]);
            conditions.file_vault = output("fdesetup", &["status"]);
            conditions.power = output("pmset", &["-g", "ps"])
                .lines()
                .next()
                .unwrap_or("unknown")
                .to_string();
            conditions.thermal = output("pmset", &["-g", "therm"]);
            conditions.spotlight = output("mdutil", &["-s", &dir_text]);
        } else {
            conditions.volume_format = output("findmnt", &["-no", "FSTYPE", "-T", &dir_text]);
            let source = output("findmnt", &["-no", "SOURCE", "-T", &dir_text]);
            let source = source.split('[').next().unwrap_or("");
            let devices = output("lsblk", &["-s", "-no", "TYPE", source]);
            conditions.file_vault = if devices.split_whitespace().any(|t| t == "crypt") {
                "dm-crypt (LUKS)".into()
            } else {
                format!(
                    "none: {}",
                    devices.split_whitespace().collect::<Vec<_>>().join(", ")
                )
            };
            conditions.model = std::fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|info| {
                    info.lines()
                        .find(|line| line.starts_with("model name"))
                        .and_then(|line| line.split_once(':'))
                        .map(|(_, name)| name.trim().to_string())
                })
                .unwrap_or_else(|| "unknown".into());
            conditions.macos_build = output("uname", &["-r"]);
            conditions.power = if Path::new("/sys/class/power_supply/BAT0").exists() {
                output("cat", &["/sys/class/power_supply/BAT0/status"])
            } else {
                "AC Power, no battery".to_string()
            };
        }
        conditions
    }

    /// Why this run's numbers shouldn't be published, if any. Only the load
    /// before counts: a suite's own threads raise the load after it, and
    /// something starting midway shows in the spread instead.
    pub fn noise(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        let limit = self.cores as f64 * BUSY_SHARE;
        if self.load_before > limit {
            reasons.push(format!(
                "load {:.1} before the run, past {limit:.1} for {} cores",
                self.load_before, self.cores
            ));
        }
        if self.power.contains("Battery") || self.power.contains("Discharging") {
            reasons.push("running on battery".to_string());
        }
        reasons
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn quiet() -> Conditions {
        Conditions {
            commit: String::new(),
            profile: "release".into(),
            hostname: "mac".into(),
            os: "macos".into(),
            model: String::new(),
            cores: 12,
            macos_build: String::new(),
            volume_format: String::new(),
            file_vault: String::new(),
            power: "Now drawing from 'AC Power'".into(),
            thermal: String::new(),
            spotlight: String::new(),
            load_before: 1.,
            load_after: 2.,
        }
    }

    #[test]
    fn a_busy_or_unplugged_run_says_so_and_a_quiet_one_doesnt() {
        assert!(quiet().noise().is_empty());
        let busy = Conditions {
            load_before: 4.,
            ..quiet()
        };
        assert_eq!(busy.noise().len(), 1);
        // Its own threads raise the load after a suite.
        let busy_after = Conditions {
            load_after: 9.,
            ..quiet()
        };
        assert!(busy_after.noise().is_empty());
        let battery = Conditions {
            power: "Now drawing from 'Battery Power'".into(),
            ..quiet()
        };
        assert_eq!(battery.noise().len(), 1);
    }
}
