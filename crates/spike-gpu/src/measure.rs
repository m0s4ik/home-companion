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

/// CPU time of the calling thread, in seconds.
pub fn thread_cpu_seconds() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime only writes into the struct we pass.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } != 0 {
        return 0.0;
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Number of threads of this process (Mesa may start its own).
pub fn thread_count() -> u32 {
    status_kb("Threads:") as u32
}

/// MemAvailable of the whole system in MB: its drop over a run includes GPU
/// buffers, which the process RSS does not show.
pub fn mem_available_mb() -> f64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemAvailable:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

/// DRM client statistics of this process from /proc/self/fdinfo (kernel
/// "drm-usage-stats"): GPU busy time per engine (`drm-engine-*`, ns) and
/// buffer memory (`drm-total-*`, `drm-resident-*`, ... in KiB). Summed over
/// the distinct `drm-client-id`s; empty when no DRM device is open (soft modes)
/// or the driver does not report them.
pub fn drm_fdinfo() -> Vec<(String, f64)> {
    let Ok(rd) = std::fs::read_dir("/proc/self/fdinfo") else { return Vec::new() };
    let mut seen = std::collections::HashSet::new();
    let mut sums: Vec<(String, f64)> = Vec::new();
    for e in rd.filter_map(|e| e.ok()) {
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        let pairs = parse_drm_fdinfo(&text);
        let Some(id) = pairs.iter().find(|(k, _)| k == "drm-client-id").map(|(_, v)| *v) else { continue };
        if !seen.insert(id as u64) {
            continue;
        }
        for (k, v) in pairs.into_iter().filter(|(k, _)| k != "drm-client-id") {
            match sums.iter_mut().find(|(k2, _)| *k2 == k) {
                Some(s) => s.1 += v,
                None => sums.push((k, v)),
            }
        }
    }
    sums
}

/// Numeric `drm-*` keys of one fdinfo file; memory sizes in KiB, times in ns.
pub fn parse_drm_fdinfo(text: &str) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else { continue };
        if !key.starts_with("drm-") {
            continue;
        }
        let mut it = rest.split_whitespace();
        let Some(Ok(v)) = it.next().map(str::parse::<f64>) else { continue };
        let v = match it.next() {
            Some("B") => v / 1024.0,
            Some("MiB") => v * 1024.0,
            Some("GiB") => v * 1024.0 * 1024.0,
            _ => v, // KiB, ns, Hz or no unit
        };
        out.push((key.to_string(), v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fdinfo_parsing() {
        let text = "pos:\t0\nflags:\t02100002\ndrm-driver:\tmsm\ndrm-client-id:\t7\n\
                    drm-engine-gpu:\t123456 ns\ndrm-cycles-gpu:\t999\ndrm-total-memory:\t2 MiB\n\
                    drm-resident-memory:\t512 KiB\ndrm-purgeable-memory:\t2048 B\n";
        let p = parse_drm_fdinfo(text);
        let get = |k: &str| p.iter().find(|(k2, _)| k2 == k).map(|(_, v)| *v);
        assert_eq!(get("drm-client-id"), Some(7.0));
        assert_eq!(get("drm-engine-gpu"), Some(123456.0));
        assert_eq!(get("drm-total-memory"), Some(2048.0));
        assert_eq!(get("drm-resident-memory"), Some(512.0));
        assert_eq!(get("drm-purgeable-memory"), Some(2.0));
        assert_eq!(get("drm-driver"), None);
        assert!(thread_cpu_seconds() >= 0.0 && thread_count() >= 1);
    }
}
