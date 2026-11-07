// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Process measurements: resident memory and CPU time.

/// Resident set size in MB (VmRSS from /proc/self/status), 0.0 if unavailable.
pub fn rss_mb() -> f64 {
    status_kb("VmRSS:") / 1024.0
}

/// Peak resident set size in MB as seen by the kernel (VmHWM).
pub fn peak_rss_mb() -> f64 {
    status_kb("VmHWM:") / 1024.0
}

fn status_kb(key: &str) -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0.0)
}

/// User + system CPU time of this process, in seconds.
pub fn cpu_seconds() -> f64 {
    // SAFETY: getrusage only writes into the struct we pass.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return 0.0;
    }
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// Local wall-clock time as (hours, minutes, seconds).
pub fn local_hms() -> (u32, u32, u32) {
    // SAFETY: time/localtime_r only write into the values we pass.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return (0, 0, 0);
        }
        (tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    }
}
