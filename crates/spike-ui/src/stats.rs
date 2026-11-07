// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Per-frame and per-second instrumentation, summarised as a table and JSON.

use std::time::Instant;

use serde_json::{Value, json};

use crate::measure;

/// What the scene changed since the last frame (bit set, see `change::*`).
pub mod change {
    pub const BOX: u8 = 1;
    pub const CLOCK: u8 = 2;
    pub const VOLUME: u8 = 4;
    pub const POSE: u8 = 8;
}

/// A frame is classed by the most expensive change it contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    First,
    Pose,
    Volume,
    Clock,
    Box,
    Idle,
}

impl Class {
    pub const ALL: [Class; 6] =
        [Class::First, Class::Pose, Class::Volume, Class::Clock, Class::Box, Class::Idle];

    pub fn from_changes(bits: u8, first: bool) -> Class {
        if first {
            Class::First
        } else if bits & change::POSE != 0 {
            Class::Pose
        } else if bits & change::VOLUME != 0 {
            Class::Volume
        } else if bits & change::CLOCK != 0 {
            Class::Clock
        } else if bits & change::BOX != 0 {
            Class::Box
        } else {
            Class::Idle
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Class::First => "first",
            Class::Pose => "pose",
            Class::Volume => "volume",
            Class::Clock => "clock",
            Class::Box => "box",
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

/// The dirty region of one frame: Slint returns non-overlapping rectangles.
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

    /// Sum of the rectangle areas (they do not overlap).
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

struct Frame {
    t_ms: f64,
    class: Class,
    /// None when the backend renders internally (winit, linuxkms).
    render_ms: Option<f64>,
    present_ms: Option<f64>,
    dirty: Option<Dirty>,
}

impl Frame {
    fn total_ms(&self) -> Option<f64> {
        Some(self.render_ms? + self.present_ms.unwrap_or(0.0))
    }
}

struct Sample {
    t_s: f64,
    rss_mb: f64,
    cpu_pct: f64,
}

pub struct Stats {
    process_start: Instant,
    fps: f64,
    pub verbose: bool,
    frames: Vec<Frame>,
    samples: Vec<Sample>,
    sample_at: (Instant, f64),
    first_frame_ms: Option<f64>,
    meta: Vec<(&'static str, Value)>,
    /// When the scene timer was started; tick times are relative to it.
    scene_start: Option<Instant>,
    /// Wall-clock time of every scene tick, ms after `scene_start`.
    ticks_ms: Vec<f64>,
}

impl Stats {
    pub fn new(process_start: Instant, fps: f64, verbose: bool) -> Self {
        Stats {
            process_start,
            fps,
            verbose,
            frames: Vec::new(),
            samples: Vec::new(),
            sample_at: (Instant::now(), measure::cpu_seconds()),
            first_frame_ms: None,
            meta: Vec::new(),
            scene_start: None,
            ticks_ms: Vec::new(),
        }
    }

    pub fn set_meta(&mut self, key: &'static str, value: Value) {
        self.meta.retain(|(k, _)| *k != key);
        self.meta.push((key, value));
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    fn now_ms(&self) -> f64 {
        self.process_start.elapsed().as_secs_f64() * 1e3
    }

    /// A frame rendered by our own platform (fbdev, headless).
    pub fn frame(&mut self, class: Class, render_ms: f64, present_ms: f64, dirty: Dirty) {
        let t_ms = self.now_ms();
        self.first_frame_ms.get_or_insert(t_ms);
        if self.verbose {
            let bb = dirty.bbox();
            eprintln!(
                "frame {:5} t={:9.1}ms {:6} render {:6.2}ms present {:6.2}ms dirty {}x{}@{},{} {} px in {} rects",
                self.frames.len(),
                t_ms,
                class.name(),
                render_ms,
                present_ms,
                bb.w,
                bb.h,
                bb.x,
                bb.y,
                dirty.area(),
                dirty.rects.len()
            );
        }
        self.frames.push(Frame {
            t_ms,
            class,
            render_ms: Some(render_ms),
            present_ms: Some(present_ms),
            dirty: Some(dirty),
        });
    }

    /// The scene timer starts now: tick times are measured from here.
    pub fn scene_started(&mut self) {
        self.scene_start = Some(Instant::now());
        self.ticks_ms.clear();
    }

    /// One scene timer callback (every backend).
    pub fn tick(&mut self) {
        let start = *self.scene_start.get_or_insert_with(Instant::now);
        self.tick_at(start.elapsed().as_secs_f64() * 1e3);
    }

    fn tick_at(&mut self, ms: f64) {
        self.ticks_ms.push(ms);
    }

    /// Wall-clock pacing of the scene timer. Slint reschedules a late repeated
    /// timer from `now`, and scene time counts ticks, so on a slow device the
    /// animation just runs slower: only the wall clock shows it.
    /// Returns (wall seconds, ticks, gaps > 1.5/fps, longest gap ms).
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

    /// A scene update on a backend that renders internally: only counted.
    pub fn tick_unmeasured(&mut self, class: Class) {
        let t_ms = self.now_ms();
        self.first_frame_ms.get_or_insert(t_ms);
        if self.verbose {
            eprintln!("tick {:5} t={:9.1}ms {}", self.frames.len(), t_ms, class.name());
        }
        self.frames.push(Frame { t_ms, class, render_ms: None, present_ms: None, dirty: None });
    }

    /// Takes an RSS/CPU sample when at least one second has passed.
    /// Returns the CPU % of one core of the last second when sampled.
    pub fn maybe_sample(&mut self) -> Option<f64> {
        let (at, cpu) = self.sample_at;
        let dt = at.elapsed().as_secs_f64();
        if dt < 1.0 {
            return None;
        }
        let cpu_now = measure::cpu_seconds();
        let cpu_pct = (cpu_now - cpu) / dt * 100.0;
        self.sample_at = (Instant::now(), cpu_now);
        self.samples.push(Sample {
            t_s: self.process_start.elapsed().as_secs_f64(),
            rss_mb: measure::rss_mb(),
            cpu_pct,
        });
        Some(cpu_pct)
    }

    /// Frame time (render + present) of the last `n` measured frames, for the status line.
    pub fn recent_ms(&self, n: usize) -> Option<(f64, f64)> {
        let v: Vec<f64> = self.frames.iter().rev().take(n).filter_map(Frame::total_ms).collect();
        if v.is_empty() {
            return None;
        }
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        Some((mean, v.iter().cloned().fold(0.0, f64::max)))
    }

    pub fn summary(&self) -> Value {
        let budget_ms = 1e3 / self.fps;
        let over_budget = self.frames.iter().filter_map(Frame::total_ms).filter(|t| *t > budget_ms).count();
        let (wall_s, ticks, late_ticks, max_gap_ms) = self.pacing();
        let per_s = |n: usize| (wall_s > 0.0).then(|| round2(n as f64 / wall_s));
        let mut classes = serde_json::Map::new();
        for class in Class::ALL {
            let frames: Vec<&Frame> = self.frames.iter().filter(|f| f.class == class).collect();
            if frames.is_empty() {
                continue;
            }
            let total: Vec<f64> = frames.iter().filter_map(|f| f.total_ms()).collect();
            let render: Vec<f64> = frames.iter().filter_map(|f| f.render_ms).collect();
            let present: Vec<f64> = frames.iter().filter_map(|f| f.present_ms).collect();
            let dirty: Vec<f64> =
                frames.iter().filter_map(|f| f.dirty.as_ref()).map(|d| d.area() as f64).collect();
            classes.insert(
                class.name().into(),
                json!({
                    "count": frames.len(),
                    "total_ms": dist(&total),
                    "render_ms": dist(&render),
                    "present_ms": dist(&present),
                    "dirty_px_mean": mean(&dirty),
                    "dirty_px_max": dirty.iter().cloned().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v)))),
                }),
            );
        }
        let cpu: Vec<f64> = self.samples.iter().map(|s| s.cpu_pct).collect();
        let mut out = serde_json::Map::new();
        for (k, v) in &self.meta {
            out.insert((*k).into(), v.clone());
        }
        out.insert("fps".into(), json!(self.fps));
        out.insert("frames_total".into(), json!(self.frames.len()));
        // Pacing: what the run really achieved on the wall clock.
        out.insert("wall_s".into(), json!(round3(wall_s)));
        out.insert("ticks_total".into(), json!(ticks));
        out.insert("achieved_tick_hz".into(), json!(per_s(ticks)));
        out.insert("achieved_fps".into(), json!(per_s(self.frames.len())));
        out.insert("late_ticks".into(), json!(late_ticks));
        out.insert("max_tick_gap_ms".into(), json!(round3(max_gap_ms)));
        // Frames whose render + present alone exceeded 1/fps (our platform only).
        out.insert("frames_over_budget".into(), json!(over_budget));
        out.insert("startup_to_first_frame_ms".into(), json!(self.first_frame_ms));
        out.insert("peak_rss_mb".into(), json!(measure::peak_rss_mb()));
        out.insert("cpu_pct_mean".into(), json!(mean(&cpu)));
        out.insert("cpu_pct_max".into(), json!(cpu.iter().cloned().fold(0.0, f64::max)));
        out.insert("classes".into(), Value::Object(classes));
        out.insert(
            "frames".into(),
            Value::Array(
                self.frames
                    .iter()
                    .map(|f| {
                        let bb = f.dirty.as_ref().map(Dirty::bbox);
                        json!({
                            "t_ms": round2(f.t_ms),
                            "class": f.class.name(),
                            "render_ms": f.render_ms.map(round3),
                            "present_ms": f.present_ms.map(round3),
                            "bbox": bb.map(|b| [b.x, b.y, b.w, b.h]),
                            "dirty_px": f.dirty.as_ref().map(Dirty::area),
                            "rects": f.dirty.as_ref().map(|d| d.rects.len()),
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
                    .map(|s| json!({"t_s": round2(s.t_s), "rss_mb": round2(s.rss_mb), "cpu_pct": round2(s.cpu_pct)}))
                    .collect(),
            ),
        );
        Value::Object(out)
    }

    pub fn print_table(&self, summary: &Value) {
        println!(
            "{:<7} {:>6} {:>9} {:>9} {:>9} {:>10} {:>10}",
            "class", "frames", "p50 ms", "p95 ms", "max ms", "dirty px", "render p50"
        );
        if let Some(classes) = summary["classes"].as_object() {
            for (name, c) in classes {
                let f = |v: &Value| v.as_f64().map_or("-".to_string(), |x| format!("{x:.2}"));
                println!(
                    "{:<7} {:>6} {:>9} {:>9} {:>9} {:>10} {:>10}",
                    name,
                    c["count"],
                    f(&c["total_ms"]["p50"]),
                    f(&c["total_ms"]["p95"]),
                    f(&c["total_ms"]["max"]),
                    c["dirty_px_mean"].as_f64().map_or("-".into(), |x| format!("{x:.0}")),
                    f(&c["render_ms"]["p50"]),
                );
            }
        }
        let f1 = |v: &Value| v.as_f64().map_or("-".to_string(), |x| format!("{x:.1}"));
        println!(
            "wall {} s  ticks {} ({} Hz of {})  achieved fps {}  late ticks (gap > 1.5/fps) {}  longest gap {} ms",
            f1(&summary["wall_s"]),
            summary["ticks_total"],
            f1(&summary["achieved_tick_hz"]),
            summary["fps"],
            f1(&summary["achieved_fps"]),
            summary["late_ticks"],
            f1(&summary["max_tick_gap_ms"]),
        );
        println!(
            "frames {}  over budget {}  first frame {} ms  peak RSS {:.1} MB  CPU mean {} % of one core",
            summary["frames_total"],
            summary["frames_over_budget"],
            summary["startup_to_first_frame_ms"].as_f64().map_or("-".into(), |v| format!("{v:.0}")),
            summary["peak_rss_mb"].as_f64().unwrap_or(0.0),
            summary["cpu_pct_mean"].as_f64().map_or("-".into(), |v| format!("{v:.1}")),
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
        // Box moved right by 10 px: Slint reports old + new as disjoint rects.
        let d = Dirty::from_rects([
            Rect { x: 100, y: 980, w: 74, h: 64 },
            Rect { x: 0, y: 0, w: 0, h: 10 }, // empty, dropped
            Rect { x: 300, y: 700, w: 20, h: 10 },
        ]);
        assert_eq!(d.rects.len(), 2);
        assert_eq!(d.area(), 74 * 64 + 200);
        assert_eq!(d.bbox(), Rect { x: 100, y: 700, w: 220, h: 344 });
        assert_eq!(Dirty::default().bbox(), Rect::default());
        let c = Dirty::from_rects([Rect { x: 790, y: 1270, w: 20, h: 20 }]).clipped(800, 1280);
        assert_eq!(c.rects, vec![Rect { x: 790, y: 1270, w: 10, h: 10 }]);
    }

    #[test]
    fn classes_by_priority() {
        use change::*;
        assert_eq!(Class::from_changes(BOX | CLOCK, false), Class::Clock);
        assert_eq!(Class::from_changes(BOX | POSE | VOLUME, false), Class::Pose);
        assert_eq!(Class::from_changes(0, false), Class::Idle);
        assert_eq!(Class::from_changes(BOX, true), Class::First);
    }

    #[test]
    fn summary_counts_frames_over_budget() {
        let mut s = Stats::new(Instant::now(), 16.0, false);
        let d = || Dirty::from_rects([Rect { x: 0, y: 0, w: 64, h: 64 }]);
        s.frame(Class::Box, 2.0, 1.0, d());
        s.frame(Class::Box, 70.0, 1.0, d()); // > 62.5 ms
        s.frame(Class::Pose, 20.0, 5.0, d());
        let j = s.summary();
        assert_eq!(j["frames_total"], 3);
        assert_eq!(j["frames_over_budget"], 1);
        assert_eq!(j["classes"]["box"]["count"], 2);
        assert_eq!(j["classes"]["pose"]["total_ms"]["max"], 25.0);
        assert_eq!(j["classes"]["box"]["dirty_px_mean"], 4096.0);
    }

    #[test]
    fn summary_pacing_sees_late_ticks() {
        let mut s = Stats::new(Instant::now(), 16.0, false); // 62.5 ms period
        // On time, then one tick 150 ms late (rescheduled from there), then on time.
        for t in [62.5, 125.0, 187.5, 337.5, 400.0, 462.5, 525.0, 587.5] {
            s.tick_at(t);
        }
        let j = s.summary();
        assert_eq!(j["ticks_total"], 8);
        assert_eq!(j["late_ticks"], 1);
        assert_eq!(j["max_tick_gap_ms"], 150.0);
        assert_eq!(j["wall_s"], 0.588);
        // 8 ticks in 0.5875 s: 13.6 Hz, not 16
        assert_eq!(j["achieved_tick_hz"], 13.62);
        assert_eq!(j["frames_over_budget"], 0);
    }
}
