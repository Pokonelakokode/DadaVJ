//! Frame-idő mérés: azonnali visszajelzés és bench-összegzés.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Saját folyamat CPU/RSS mérése (getrusage). A ps a sandboxban nem elérhető,
/// és amúgy is hasznos, ha az app maga mutatja a költségét.
pub struct ProcStats {
    last_cpu_secs: f64,
    last_wall: Instant,
    pub cpu_percent: f32,
    pub rss_mb: f32,
}

fn cpu_seconds() -> f64 {
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut u) == 0 {
            u.ru_utime.tv_sec as f64
                + u.ru_utime.tv_usec as f64 / 1e6
                + u.ru_stime.tv_sec as f64
                + u.ru_stime.tv_usec as f64 / 1e6
        } else {
            0.0
        }
    }
}

fn max_rss_mb() -> f32 {
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut u) == 0 {
            // macOS-en bájtban, Linuxon KB-ban.
            #[cfg(target_os = "macos")]
            {
                u.ru_maxrss as f32 / (1024.0 * 1024.0)
            }
            #[cfg(not(target_os = "macos"))]
            {
                u.ru_maxrss as f32 / 1024.0
            }
        } else {
            0.0
        }
    }
}

impl Default for ProcStats {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcStats {
    pub fn new() -> Self {
        Self {
            last_cpu_secs: cpu_seconds(),
            last_wall: Instant::now(),
            cpu_percent: 0.0,
            rss_mb: max_rss_mb(),
        }
    }

    /// Frissíti a CPU%-ot és az RSS-t (másodpercenként hívandó).
    pub fn sample(&mut self) {
        let now = Instant::now();
        let wall = now.duration_since(self.last_wall).as_secs_f64();
        let cpu = cpu_seconds();
        if wall > 0.05 {
            self.cpu_percent = ((cpu - self.last_cpu_secs) / wall * 100.0) as f32;
        }
        self.last_cpu_secs = cpu;
        self.last_wall = now;
        self.rss_mb = max_rss_mb();
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub frames: u64,
    pub avg_ms: f64,
    pub fps: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub over_budget: u64,
}

pub struct FrameStats {
    samples: VecDeque<f32>,
    since_report: Vec<f32>,
    last_report: Instant,
    report_interval: Duration,
    total_frames: u64,
    over_budget: u64,
}

impl FrameStats {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            since_report: Vec::with_capacity(256),
            last_report: Instant::now(),
            report_interval: Duration::from_secs(1),
            total_frames: 0,
            over_budget: 0,
        }
    }

    pub fn push(&mut self, ms: f32) {
        self.total_frames += 1;
        // A vsync-es present jittere (±0.5 ms) nem hiba; 1.5x büdzsé felett számolunk.
        if ms > (1000.0 / 60.0) * 1.5 {
            self.over_budget += 1;
        }
        self.since_report.push(ms);
        if self.samples.len() == self.samples.capacity() {
            self.samples.pop_front();
        }
        self.samples.push_back(ms);
    }

    /// Másodpercenként egyszer ad riportot az aktuális másodpercről.
    pub fn take_report(&mut self) -> Option<Report> {
        if self.last_report.elapsed() < self.report_interval || self.since_report.is_empty() {
            return None;
        }
        let mut v = std::mem::take(&mut self.since_report);
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let avg = v.iter().sum::<f32>() / v.len() as f32;
        let report = Report {
            fps: if avg > 0.0 { 1000.0 / avg as f64 } else { 0.0 },
            avg_ms: avg as f64,
            min_ms: v[0] as f64,
            max_ms: v[v.len() - 1] as f64,
            count: v.len(),
        };
        self.last_report = Instant::now();
        Some(report)
    }

    pub fn summary(&self) -> Option<Summary> {
        if self.samples.is_empty() {
            return None;
        }
        let mut v: Vec<f32> = self.samples.iter().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let avg = v.iter().map(|x| *x as f64).sum::<f64>() / v.len() as f64;
        let pct = |p: f64| -> f64 {
            let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
            v[idx] as f64
        };
        Some(Summary {
            frames: self.total_frames,
            avg_ms: avg,
            fps: if avg > 0.0 { 1000.0 / avg } else { 0.0 },
            p50_ms: pct(0.50),
            p95_ms: pct(0.95),
            p99_ms: pct(0.99),
            max_ms: v[v.len() - 1] as f64,
            over_budget: self.over_budget,
        })
    }
}

pub struct Report {
    pub fps: f64,
    pub avg_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub count: usize,
}

impl Report {
    pub fn line(&self) -> String {
        format!(
            "  {:5.1} fps | átlag {:6.2} ms | min {:6.2} | max {:6.2} | {} frame/s",
            self.fps, self.avg_ms, self.min_ms, self.max_ms, self.count
        )
    }

    pub fn line_with_proc(&self, proc: &ProcStats) -> String {
        format!(
            "{} | CPU {:5.1}% | RSS {:5.0} MB",
            self.line().trim(),
            proc.cpu_percent,
            proc.rss_mb
        )
    }
}
