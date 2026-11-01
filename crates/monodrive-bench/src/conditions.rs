//! What a run was measured under, recorded so a number can be judged later.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conditions {
    pub commit: String,
    pub model: String,
    pub cores: usize,
    pub macos_build: String,
    pub volume_format: String,
    pub file_vault: String,
    pub power: String,
    pub thermal: String,
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

impl Conditions {
    pub fn now(volume: &Path) -> Self {
        let volume_text = volume.to_string_lossy();
        let mount = output("mount", &[]);
        let volume_format = mount
            .lines()
            .find(|line| line.contains(&format!(" on {volume_text} (")))
            .and_then(|line| line.rsplit_once(" (")?.1.split(',').next())
            .unwrap_or("unknown")
            .to_string();
        Self {
            commit: output("git", &["rev-parse", "--short", "HEAD"]),
            model: output("sysctl", &["-n", "hw.model"]),
            cores: std::thread::available_parallelism().map_or(0, usize::from),
            macos_build: output("sw_vers", &["-buildVersion"]),
            volume_format,
            file_vault: output("fdesetup", &["status"]),
            power: output("pmset", &["-g", "ps"])
                .lines()
                .next()
                .unwrap_or("unknown")
                .to_string(),
            thermal: output("pmset", &["-g", "therm"]),
            spotlight: output("mdutil", &["-s", &volume_text]),
            load_before: load(),
            load_after: f64::NAN,
        }
    }

    /// Why this run's numbers shouldn't be published, if any.
    pub fn noise(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        let limit = self.cores as f64 / 4.;
        for (when, load) in [("before", self.load_before), ("after", self.load_after)] {
            if load > limit {
                reasons.push(format!(
                    "load {load:.1} {when} the run, past {limit:.1} for {} cores",
                    self.cores
                ));
            }
        }
        if self.power.contains("Battery") {
            reasons.push("running on battery".to_string());
        }
        reasons
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conditions(load_after: f64, power: &str) -> Conditions {
        Conditions {
            commit: String::new(),
            model: String::new(),
            cores: 12,
            macos_build: String::new(),
            volume_format: String::new(),
            file_vault: String::new(),
            power: power.to_string(),
            thermal: String::new(),
            spotlight: String::new(),
            load_before: 1.,
            load_after,
        }
    }

    #[test]
    fn a_busy_or_unplugged_run_says_so_and_a_quiet_one_doesnt() {
        let plugged = "Now drawing from 'AC Power'";
        assert!(conditions(2., plugged).noise().is_empty());
        assert_eq!(conditions(4., plugged).noise().len(), 1);
        let battery = "Now drawing from 'Battery Power'";
        assert_eq!(conditions(2., battery).noise().len(), 1);
    }
}
