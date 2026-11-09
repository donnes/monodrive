//! One sample: wall time, CPU time, and on macOS the instructions and cycles
//! `proc_pid_rusage` counts, for work in this process or in a child.
//!
//! The counters cover the whole process, every thread, kernel work included.
//! They don't see time spent blocked on a lock or a flush, so they guard
//! regressions beside wall time and don't replace it.

use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub wall_ms: f64,
    /// User and system time. `None` for a child on Linux, which only
    /// reports it in a total once reaped.
    pub cpu_ms: Option<f64>,
    /// `None` where the platform has no per-process counters, or they read 0.
    pub instructions: Option<u64>,
    pub cycles: Option<u64>,
}

impl Sample {
    /// This and `next`, done one after the other, as one sample. A counter
    /// either lacks stays unknown.
    pub fn then(self, next: Sample) -> Sample {
        let both = |a: Option<u64>, b: Option<u64>| Some(a? + b?);
        Sample {
            wall_ms: self.wall_ms + next.wall_ms,
            cpu_ms: self.cpu_ms.zip(next.cpu_ms).map(|(a, b)| a + b),
            instructions: both(self.instructions, next.instructions),
            cycles: both(self.cycles, next.cycles),
        }
    }
}

#[derive(Clone, Copy)]
struct Usage {
    cpu_ns: u64,
    instructions: u64,
    cycles: u64,
}

impl Usage {
    fn since(self, before: Usage, wall_ms: f64) -> Sample {
        let counted =
            |after: u64, before: u64| Some(after.saturating_sub(before)).filter(|n| *n > 0);
        Sample {
            wall_ms,
            cpu_ms: Some(self.cpu_ns.saturating_sub(before.cpu_ns) as f64 / 1e6),
            instructions: counted(self.instructions, before.instructions),
            cycles: counted(self.cycles, before.cycles),
        }
    }
}

#[cfg(target_os = "macos")]
fn usage(pid: libc::pid_t) -> Option<Usage> {
    use std::sync::OnceLock;
    // `ri_user_time` and `ri_system_time` are in Mach ticks, which aren't
    // nanoseconds on Apple silicon.
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = Timebase { numer: 1, denom: 1 };
        // SAFETY: fills the struct it's given.
        unsafe { mach_timebase_info(&mut info) };
        (u64::from(info.numer), u64::from(info.denom.max(1)))
    });
    // SAFETY: an all-zero rusage_info_v4 is valid, and the call writes only
    // into it.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let read = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        )
    };
    (read == 0).then(|| Usage {
        cpu_ns: (info.ri_user_time + info.ri_system_time) * numer / denom,
        instructions: info.ri_instructions,
        cycles: info.ri_cycles,
    })
}

#[cfg(not(target_os = "macos"))]
fn usage(pid: libc::pid_t) -> Option<Usage> {
    // Linux has no counters without perf_event_open; CPU time only.
    (pid == std::process::id() as libc::pid_t).then(|| {
        // SAFETY: an all-zero rusage is valid, and the call writes only into it.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        Usage {
            cpu_ns: nanos(usage.ru_utime) + nanos(usage.ru_stime),
            instructions: 0,
            cycles: 0,
        }
    })
}

#[cfg(not(target_os = "macos"))]
fn nanos(time: libc::timeval) -> u64 {
    time.tv_sec as u64 * 1_000_000_000 + time.tv_usec as u64 * 1_000
}

fn own() -> Usage {
    usage(std::process::id() as libc::pid_t).unwrap_or(Usage {
        cpu_ns: 0,
        instructions: 0,
        cycles: 0,
    })
}

/// Times `work` in this process.
pub fn here<T>(work: impl FnOnce() -> T) -> (Sample, T) {
    let before = own();
    let started = Instant::now();
    let out = work();
    let wall_ms = started.elapsed().as_secs_f64() * 1e3;
    (own().since(before, wall_ms), out)
}

/// Times `work`, which waits on the running process `pid`, such as a server
/// answering a request, and counts what `pid` did meanwhile.
pub fn served<T>(pid: u32, work: impl FnOnce() -> T) -> (Sample, T) {
    let before = usage(pid as libc::pid_t);
    let started = Instant::now();
    let out = work();
    let wall_ms = started.elapsed().as_secs_f64() * 1e3;
    let sample = match (before, usage(pid as libc::pid_t)) {
        (Some(before), Some(after)) => after.since(before, wall_ms),
        _ => Sample {
            wall_ms,
            cpu_ms: None,
            instructions: None,
            cycles: None,
        },
    };
    (sample, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_here_takes_time() {
        let (sample, sum) = here(|| (0..2_000_000u64).fold(0u64, |a, b| a ^ b.wrapping_mul(31)));
        assert!(sum > 0 && sample.wall_ms > 0.);
    }
}
