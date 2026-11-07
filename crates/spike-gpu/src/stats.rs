// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Per-frame and per-second instrumentation, summarised as a table and JSON
//! (the same shape as spike-ui's, plus frame intervals, CPU per frame and the
//! DRM client statistics of the GPU path).

use std::time::Instant;

use serde_json::{Value, json};

use crate::measure;

/// What the scene changed since the last frame (bit set).
pub mod change {
    pub const ANIM: u8 = 1;
    pub const CLOCK: u8 = 2;
    pub const POSE: u8 = 4;
}

/// A frame is classed by the most expensive change it contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    First,
    Pose,
    Clock,
    Anim,
    Idle,
}

impl Class {
    pub const ALL: [Class; 5] = [Class::First, Class::Pose, Class::Clock, Class::Anim, Class::Idle];

    pub fn from_changes(bits: u8, first: bool) -> Class {
        if first {
            Class::First
        } else if bits & change::POSE != 0 {
            Class::Pose
        } else if bits & change::CLOCK != 0 {
            Class::Clock
        } else if bits & change::ANIM != 0 {
            Class::Anim
        } else {
            Class::Idle
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Class::First => "first",
            Class::Pose => "pose",
            Class::Clock => "clock",
            Class::Anim => "anim",
            Class::Idle => "idle",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }
}

/// The dirty region of one frame: non-overlapping rectangles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dirty {
    pub rects: Vec<Rect>,
}

impl Dirty {
    pub fn from_rects(rects: impl IntoIterator<Item = Rect>) -> Self {
        Dirty { rects: rects.into_iter().filter(|r| r.w > 0 && r.h > 0).collect() }
    }

    pub fn from_region(region: &slint::platform::software_renderer::PhysicalRegion) -> Self {
        Self::from_rects(region.iter().map(|(p, s)| Rect {
            x: p.x.max(0) as u32,
            y: p.y.max(0) as u32,
            w: s.width,
            h: s.height,
        }))
    }

    pub fn area(&self) -> u64 {
        self.rects.iter().map(Rect::area).sum()
    }

    pub fn bbox(&self) -> Rect {
        let Some(first) = self.rects.first() else { return Rect::default() };
        let (mut x0, mut y0) = (first.x, first.y);
        let (mut x1, mut y1) = (first.x + first.w, first.y + first.h);
        for r in &self.rects[1..] {
            x0 = x0.min(r.x);
            y0 = y0.min(r.y);
            x1 = x1.max(r.x + r.w);
            y1 = y1.max(r.y + r.h);
        }
        Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }

    /// Clips every rectangle to a `width` x `height` surface.
    pub fn clipped(&self, width: u32, height: u32) -> Dirty {
        Self::from_rects(self.rects.iter().map(|r| {
            let x = r.x.min(width);
            let y = r.y.min(height);
            Rect { x, y, w: (r.x + r.w).min(width) - x, h: (r.y + r.h).min(height) - y }
        }))
    }
}

/// What a mode measured for one frame.
#[derive(Clone, Debug, Default)]
pub struct FrameRec {
    /// Slint rendering: software render into RAM, or femtovg recording and
    /// submitting the GL commands (BeforeRendering -> AfterRendering).
    pub render_ms: Option<f64>,
    /// Copy of Slint's dirty rows to the framebuffer (soft modes).
    pub present_ms: Option<f64>,
    /// soft-blit: copy of the pre-converted character frame into the framebuffer.
    pub blit_ms: Option<f64>,
    /// CPU time of the drawing thread over render + present + blit.
    pub thread_cpu_ms: Option<f64>,
    pub dirty: Option<Dirty>,
}

struct Frame {
    t_ms: f64,
    class: Class,
    rec: FrameRec,
    /// Wall time since the previous frame.
    interval_ms: Option<f64>,
    /// CPU time of the whole process (every thread) since the previous frame.
    cpu_ms: Option<f64>,
}

impl Frame {
    /// Work the mode did for this frame on the drawing thread (wall).
    fn total_ms(&self) -> Option<f64> {
        let r = &self.rec;
        if r.render_ms.is_none() && r.blit_ms.is_none() {
            return None;
        }
        Some(r.render_ms.unwrap_or(0.0) + r.present_ms.unwrap_or(0.0) + r.blit_ms.unwrap_or(0.0))
    }
}

struct Sample {
    t_s: f64,
    rss_mb: f64,
    cpu_pct: f64,
    gpu_pct: Option<f64>,
    threads: u32,
}

pub struct Stats {
    process_start: Instant,
    fps: f64,
    pub verbose: bool,
    frames: Vec<Frame>,
    samples: Vec<Sample>,
    /// (when, process CPU s, GPU busy ns) of the last sample.
    sample_at: (Instant, f64, Option<f64>),
    /// (when, process CPU s) of the last frame.
    last_frame: Option<(Instant, f64)>,
    first_frame_ms: Option<f64>,
    meta: Vec<(&'static str, Value)>,
    scene_start: Option<Instant>,
    ticks_ms: Vec<f64>,
    mem_available_start_mb: f64,
    min_mem_available_mb: f64,
}

fn gpu_busy_ns(drm: &[(String, f64)]) -> Option<f64> {
    let v: Vec<f64> = drm.iter().filter(|(k, _)| k.starts_with("drm-engine-")).map(|(_, v)| *v).collect();
    (!v.is_empty()).then(|| v.iter().sum())
}

impl Stats {
    pub fn new(process_start: Instant, fps: f64, verbose: bool) -> Self {
        let avail = measure::mem_available_mb();
        Stats {
            process_start,
            fps,
            verbose,
            frames: Vec::new(),
            samples: Vec::new(),
            sample_at: (Instant::now(), measure::cpu_seconds(), None),
            last_frame: None,
            first_frame_ms: None,
            meta: Vec::new(),
            scene_start: None,
            ticks_ms: Vec::new(),
            mem_available_start_mb: avail,
            min_mem_available_mb: avail,
        }
    }

    pub fn set_meta(&mut self, key: &'static str, value: Value) {
        self.meta.retain(|(k, _)| *k != key);
        self.meta.push((key, value));
    }

    fn now_ms(&self) -> f64 {
        self.process_start.elapsed().as_secs_f64() * 1e3
    }

    /// One frame on the screen (or submitted to it).
    pub fn frame(&mut self, class: Class, rec: FrameRec) {
        let now = Instant::now();
        let cpu = measure::cpu_seconds();
        let t_ms = self.now_ms();
        if self.first_frame_ms.is_none() {
            self.first_frame_ms = Some(t_ms);
            // Pacing counts from the first frame on: loading the frames and the
            // GPU mode's texture upload are startup, not late ticks.
            self.scene_start = Some(now);
            self.ticks_ms.clear();
            // Likewise the per-second CPU/GPU samples start here, after loading.
            self.sample_at = (now, cpu, gpu_busy_ns(&measure::drm_fdinfo()));
        }
        let (interval_ms, cpu_ms) = match self.last_frame {
            Some((t, c)) => (Some((now - t).as_secs_f64() * 1e3), Some((cpu - c) * 1e3)),
            None => (None, None),
        };
        self.last_frame = Some((now, cpu));
        if self.verbose {
            let f = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.2}"));
            let dirty = rec.dirty.as_ref().map_or(String::new(), |d| {
                let bb = d.bbox();
                format!(" dirty {}x{}@{},{} {} px", bb.w, bb.h, bb.x, bb.y, d.area())
            });
            eprintln!(
                "frame {:5} t={:9.1}ms {:5} render {} present {} blit {} thread cpu {} | interval {} process cpu {}{dirty}",
                self.frames.len(),
                t_ms,
                class.name(),
                f(rec.render_ms),
                f(rec.present_ms),
                f(rec.blit_ms),
                f(rec.thread_cpu_ms),
                f(interval_ms),
                f(cpu_ms),
            );
        }
        self.frames.push(Frame { t_ms, class, rec, interval_ms, cpu_ms });
    }

    /// The scene timer starts now: tick times are measured from here.
    pub fn scene_started(&mut self) {
        self.scene_start = Some(Instant::now());
        self.ticks_ms.clear();
    }

    /// One scene timer callback.
    pub fn tick(&mut self) {
        let start = *self.scene_start.get_or_insert_with(Instant::now);
        self.tick_at(start.elapsed().as_secs_f64() * 1e3);
    }

    fn tick_at(&mut self, ms: f64) {
        self.ticks_ms.push(ms);
    }

    /// Wall-clock pacing of the scene timer: (wall s, ticks, gaps > 1.5/fps, longest gap ms).
    fn pacing(&self) -> (f64, usize, usize, f64) {
        let period_ms = 1e3 / self.fps;
        let mut prev = 0.0;
        let (mut late, mut max_gap) = (0, 0.0f64);
        for &t in &self.ticks_ms {
            let gap = t - prev;
            if gap > 1.5 * period_ms {
                late += 1;
            }
            max_gap = max_gap.max(gap);
            prev = t;
        }
        (prev / 1e3, self.ticks_ms.len(), late, max_gap)
    }

    /// Takes an RSS/CPU/GPU sample when at least one second has passed.
    pub fn maybe_sample(&mut self) {
        let (at, cpu, gpu) = self.sample_at;
        let dt = at.elapsed().as_secs_f64();
        if dt < 1.0 {
            return;
        }
        let cpu_now = measure::cpu_seconds();
        let gpu_now = gpu_busy_ns(&measure::drm_fdinfo());
        let gpu_pct = match (gpu, gpu_now) {
            (Some(a), Some(b)) => Some((b - a) / 1e9 / dt * 100.0),
            _ => None,
        };
        self.sample_at = (Instant::now(), cpu_now, gpu_now);
        self.min_mem_available_mb = self.min_mem_available_mb.min(measure::mem_available_mb());
        self.samples.push(Sample {
            t_s: self.process_start.elapsed().as_secs_f64(),
            rss_mb: measure::rss_mb(),
            cpu_pct: (cpu_now - cpu) / dt * 100.0,
            gpu_pct,
            threads: measure::thread_count(),
        });
    }

    pub fn summary(&self) -> Value {
        let budget_ms = 1e3 / self.fps;
        let over_budget = self.frames.iter().filter_map(Frame::total_ms).filter(|t| *t > budget_ms).count();
        let (wall_s, ticks, late_ticks, max_gap_ms) = self.pacing();
        let per_s = |n: usize| (wall_s > 0.0).then(|| round2(n as f64 / wall_s));
        let pick = |frames: &[&Frame], f: &dyn Fn(&Frame) -> Option<f64>| -> Vec<f64> {
            frames.iter().filter_map(|x| f(x)).collect()
        };
        let mut classes = serde_json::Map::new();
        for class in Class::ALL {
            let frames: Vec<&Frame> = self.frames.iter().filter(|f| f.class == class).collect();
            if frames.is_empty() {
                continue;
            }
            let dirty = pick(&frames, &|f| f.rec.dirty.as_ref().map(|d| d.area() as f64));
            classes.insert(
                class.name().into(),
                json!({
                    "count": frames.len(),
                    "total_ms": dist(&pick(&frames, &Frame::total_ms)),
                    "render_ms": dist(&pick(&frames, &|f| f.rec.render_ms)),
                    "present_ms": dist(&pick(&frames, &|f| f.rec.present_ms)),
                    "blit_ms": dist(&pick(&frames, &|f| f.rec.blit_ms)),
                    "thread_cpu_ms": dist(&pick(&frames, &|f| f.rec.thread_cpu_ms)),
                    "process_cpu_ms": dist(&pick(&frames, &|f| f.cpu_ms)),
                    "interval_ms": dist(&pick(&frames, &|f| f.interval_ms)),
                    "dirty_px_mean": mean(&dirty),
                }),
            );
        }
        let all: Vec<&Frame> = self.frames.iter().collect();
        // Steady state: everything after the first frame (which includes uploads/setup).
        let steady: Vec<&Frame> = self.frames.iter().skip(1).collect();
        let cpu: Vec<f64> = self.samples.iter().map(|s| s.cpu_pct).collect();
        let gpu: Vec<f64> = self.samples.iter().filter_map(|s| s.gpu_pct).collect();
        let mut out = serde_json::Map::new();
        for (k, v) in &self.meta {
            out.insert((*k).into(), v.clone());
        }
        out.insert("fps".into(), json!(self.fps));
        out.insert("frames_total".into(), json!(self.frames.len()));
        out.insert("wall_s".into(), json!(round3(wall_s)));
        out.insert("ticks_total".into(), json!(ticks));
        out.insert("achieved_tick_hz".into(), json!(per_s(ticks)));
        out.insert("achieved_fps".into(), json!(per_s(self.frames.len())));
        out.insert("late_ticks".into(), json!(late_ticks));
        out.insert("max_tick_gap_ms".into(), json!(round3(max_gap_ms)));
        out.insert("frames_over_budget".into(), json!(over_budget));
        out.insert("frame_interval_ms".into(), dist(&pick(&steady, &|f| f.interval_ms)));
        out.insert("frame_total_ms".into(), dist(&pick(&steady, &Frame::total_ms)));
        out.insert("frame_thread_cpu_ms".into(), dist(&pick(&steady, &|f| f.rec.thread_cpu_ms)));
        out.insert("frame_process_cpu_ms".into(), dist(&pick(&steady, &|f| f.cpu_ms)));
        out.insert("startup_to_first_frame_ms".into(), json!(self.first_frame_ms.map(round2)));
        out.insert("first_frame".into(), json!(all.first().map(|f| json!({
            "render_ms": f.rec.render_ms.map(round3),
            "thread_cpu_ms": f.rec.thread_cpu_ms.map(round3),
        }))));
        out.insert("peak_rss_mb".into(), json!(round2(measure::peak_rss_mb())));
        out.insert("rss_end_mb".into(), json!(round2(measure::rss_mb())));
        out.insert("cpu_pct_mean".into(), json!(mean(&cpu).map(round2)));
        out.insert("cpu_pct_max".into(), json!(round2(cpu.iter().cloned().fold(0.0, f64::max))));
        out.insert("gpu_busy_pct_mean".into(), json!(mean(&gpu).map(round2)));
        out.insert("threads".into(), json!(measure::thread_count()));
        out.insert("mem_available_start_mb".into(), json!(round2(self.mem_available_start_mb)));
        out.insert(
            "mem_available_drop_max_mb".into(),
            json!(round2(self.mem_available_start_mb - self.min_mem_available_mb.min(measure::mem_available_mb()))),
        );
        let drm: serde_json::Map<String, Value> =
            measure::drm_fdinfo().into_iter().map(|(k, v)| (k, json!(v))).collect();
        out.insert("drm_fdinfo_end".into(), Value::Object(drm));
        out.insert("classes".into(), Value::Object(classes));
        out.insert(
            "frames".into(),
            Value::Array(
                self.frames
                    .iter()
                    .map(|f| {
                        let bb = f.rec.dirty.as_ref().map(Dirty::bbox);
                        json!({
                            "t_ms": round2(f.t_ms),
                            "class": f.class.name(),
                            "render_ms": f.rec.render_ms.map(round3),
                            "present_ms": f.rec.present_ms.map(round3),
                            "blit_ms": f.rec.blit_ms.map(round3),
                            "thread_cpu_ms": f.rec.thread_cpu_ms.map(round3),
                            "interval_ms": f.interval_ms.map(round3),
                            "process_cpu_ms": f.cpu_ms.map(round3),
                            "bbox": bb.map(|b| [b.x, b.y, b.w, b.h]),
                            "dirty_px": f.rec.dirty.as_ref().map(Dirty::area),
                        })
                    })
                    .collect(),
            ),
        );
        out.insert(
            "seconds".into(),
            Value::Array(
                self.samples
                    .iter()
                    .map(|s| {
                        json!({"t_s": round2(s.t_s), "rss_mb": round2(s.rss_mb), "cpu_pct": round2(s.cpu_pct),
                               "gpu_busy_pct": s.gpu_pct.map(round2), "threads": s.threads})
                    })
                    .collect(),
            ),
        );
        Value::Object(out)
    }

    pub fn print_table(&self, summary: &Value) {
        let f = |v: &Value| v.as_f64().map_or("-".to_string(), |x| format!("{x:.2}"));
        println!(
            "{:<6} {:>6} {:>10} {:>10} {:>10} {:>12} {:>12} {:>10}",
            "class", "frames", "work p50", "work p95", "work max", "thr cpu p50", "proc cpu p50", "dirty px"
        );
        if let Some(classes) = summary["classes"].as_object() {
            for (name, c) in classes {
                println!(
                    "{:<6} {:>6} {:>10} {:>10} {:>10} {:>12} {:>12} {:>10}",
                    name,
                    c["count"],
                    f(&c["total_ms"]["p50"]),
                    f(&c["total_ms"]["p95"]),
                    f(&c["total_ms"]["max"]),
                    f(&c["thread_cpu_ms"]["p50"]),
                    f(&c["process_cpu_ms"]["p50"]),
                    c["dirty_px_mean"].as_f64().map_or("-".into(), |x| format!("{x:.0}")),
                );
            }
        }
        let f1 = |v: &Value| v.as_f64().map_or("-".to_string(), |x| format!("{x:.1}"));
        println!(
            "wall {} s  ticks {} ({} Hz of {})  achieved fps {}  late ticks {}  longest gap {} ms  frame interval p50/p95/max {}/{}/{} ms",
            f1(&summary["wall_s"]),
            summary["ticks_total"],
            f1(&summary["achieved_tick_hz"]),
            summary["fps"],
            f1(&summary["achieved_fps"]),
            summary["late_ticks"],
            f1(&summary["max_tick_gap_ms"]),
            f1(&summary["frame_interval_ms"]["p50"]),
            f1(&summary["frame_interval_ms"]["p95"]),
            f1(&summary["frame_interval_ms"]["max"]),
        );
        println!(
            "frames {}  over budget {}  first frame at {} ms  peak RSS {} MB  CPU mean {} % of one core  GPU busy {} %  MemAvailable drop {} MB",
            summary["frames_total"],
            summary["frames_over_budget"],
            f1(&summary["startup_to_first_frame_ms"]),
            f1(&summary["peak_rss_mb"]),
            f1(&summary["cpu_pct_mean"]),
            f1(&summary["gpu_busy_pct_mean"]),
            f1(&summary["mem_available_drop_max_mb"]),
        );
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// p50 / p95 / max of a sample (nearest rank), or null when empty.
pub fn dist(v: &[f64]) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| s[((p / 100.0) * (s.len() - 1) as f64).round() as usize];
    json!({"p50": round3(pct(50.0)), "p95": round3(pct(95.0)), "max": round3(s[s.len() - 1])})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_bookkeeping() {
        let d = Dirty::from_rects([
            Rect { x: 180, y: 40, w: 440, h: 760 },
            Rect { x: 0, y: 0, w: 0, h: 10 },
            Rect { x: 0, y: 1050, w: 800, h: 200 },
        ]);
        assert_eq!(d.rects.len(), 2);
        assert_eq!(d.area(), 440 * 760 + 800 * 200);
        assert_eq!(d.bbox(), Rect { x: 0, y: 40, w: 800, h: 1210 });
        let c = Dirty::from_rects([Rect { x: 790, y: 1270, w: 20, h: 20 }]).clipped(800, 1280);
        assert_eq!(c.rects, vec![Rect { x: 790, y: 1270, w: 10, h: 10 }]);
    }

    #[test]
    fn classes_by_priority() {
        use change::*;
        assert_eq!(Class::from_changes(ANIM | CLOCK, false), Class::Clock);
        assert_eq!(Class::from_changes(ANIM | POSE | CLOCK, false), Class::Pose);
        assert_eq!(Class::from_changes(0, false), Class::Idle);
        assert_eq!(Class::from_changes(ANIM, true), Class::First);
    }

    #[test]
    fn summary_counts() {
        let mut s = Stats::new(Instant::now(), 16.0, false);
        let rec = |r: f64, b: Option<f64>| FrameRec {
            render_ms: Some(r),
            present_ms: Some(1.0),
            blit_ms: b,
            ..Default::default()
        };
        s.frame(Class::First, rec(30.0, None));
        s.frame(Class::Anim, rec(2.0, Some(1.0)));
        s.frame(Class::Anim, rec(70.0, None)); // > 62.5 ms
        s.frame(Class::Pose, rec(20.0, Some(4.0)));
        let j = s.summary();
        assert_eq!(j["frames_total"], 4);
        assert_eq!(j["frames_over_budget"], 1);
        assert_eq!(j["classes"]["anim"]["count"], 2);
        assert_eq!(j["classes"]["pose"]["total_ms"]["max"], 25.0);
        assert_eq!(j["classes"]["anim"]["blit_ms"]["max"], 1.0);
        assert!(j["frame_interval_ms"]["p50"].is_number());
        assert!(j["classes"]["first"]["interval_ms"].is_null());
    }

    #[test]
    fn summary_pacing_sees_late_ticks() {
        let mut s = Stats::new(Instant::now(), 16.0, false);
        for t in [62.5, 125.0, 187.5, 337.5, 400.0, 462.5, 525.0, 587.5] {
            s.tick_at(t);
        }
        let j = s.summary();
        assert_eq!(j["ticks_total"], 8);
        assert_eq!(j["late_ticks"], 1);
        assert_eq!(j["max_tick_gap_ms"], 150.0);
        assert_eq!(j["achieved_tick_hz"], 13.62);
    }
}
