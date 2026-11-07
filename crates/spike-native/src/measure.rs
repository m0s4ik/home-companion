// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! CPU time and memory of this process (duplicated in each spike on purpose, see M0 plan).

/// User + system CPU seconds used by this process so far.
pub fn cpu_seconds() -> f64 {
    // SAFETY: getrusage only writes into the struct we pass.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

fn status_kb(key: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with(key))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Current resident set size in MB.
pub fn rss_mb() -> f64 {
    status_kb("VmRSS:").unwrap_or(0) as f64 / 1024.0
}

/// Peak resident set size in MB.
pub fn peak_rss_mb() -> f64 {
    status_kb("VmHWM:").unwrap_or(0) as f64 / 1024.0
}
