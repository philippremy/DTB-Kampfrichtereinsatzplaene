//! Progress of the debugger's long operations, in numbers a UI can draw a real bar from.
//!
//! One [`Progress`] is shared between the worker (which reports) and the view (which polls [`Progress::snapshot`] on
//! its repaint tick). It carries the current **stage** (`2 of 4 — Loading symbols`) with an item counter (`n / m`),
//! and optionally one **transfer** running inside it (a download: bytes done / total, a smoothed speed). From those
//! the snapshot derives the fraction, the speed and an ETA — and only when they mean something: an ETA needs a
//! second or so of history, a fraction needs a known total.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long an operation must have run before its ETA is shown (earlier estimates jump around too much).
const ETA_AFTER: Duration = Duration::from_millis(1500);
/// Smallest interval between two speed samples.
const SAMPLE_EVERY: Duration = Duration::from_millis(200);

pub struct Progress {
    inner: Mutex<Inner>,
}

struct Inner {
    stage: String,
    stage_no: usize,
    stage_of: usize,
    done: u64,
    total: u64,
    label: String,
    started: Instant,
    transfer: Option<Transfer>,
}

struct Transfer {
    label: String,
    done: u64,
    total: Option<u64>,
    started: Instant,
    last_at: Instant,
    last_done: u64,
    ema_bps: Option<f64>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                stage: String::new(),
                stage_no: 0,
                stage_of: 0,
                done: 0,
                total: 0,
                label: String::new(),
                started: Instant::now(),
                transfer: None,
            }),
        }
    }
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Starts stage `no` of `of` with `total` items (0 = not countable), forgetting the previous stage.
    pub fn begin_stage(&self, no: usize, of: usize, name: &str, total: u64) {
        log::debug!("stage {no}/{of}: {name} ({total} item(s))");
        let mut s = self.inner.lock().unwrap();
        s.stage = name.to_owned();
        s.stage_no = no;
        s.stage_of = of;
        s.done = 0;
        s.total = total;
        s.label.clear();
        s.started = Instant::now();
        s.transfer = None;
    }

    /// More items than announced (a step that turned out to need more work).
    pub fn add_total(&self, by: u64) {
        self.inner.lock().unwrap().total += by;
    }

    /// What is being worked on right now (a module name, a file).
    pub fn set_label(&self, label: &str) {
        self.inner.lock().unwrap().label = label.to_owned();
    }

    pub fn advance(&self, by: u64) {
        let mut s = self.inner.lock().unwrap();
        s.done += by;
        if s.total > 0 {
            s.done = s.done.min(s.total);
        }
    }

    /// The stage is over: the bar is full whatever the count said (a skipped step counts as done).
    pub fn finish_stage(&self) {
        let mut s = self.inner.lock().unwrap();
        if s.total > 0 {
            s.done = s.total;
        }
        s.transfer = None;
    }

    /// Nothing is running any more.
    pub fn clear(&self) {
        let mut s = self.inner.lock().unwrap();
        s.stage.clear();
        s.stage_no = 0;
        s.stage_of = 0;
        s.done = 0;
        s.total = 0;
        s.label.clear();
        s.transfer = None;
    }

    /// A download (or any byte transfer) starts inside the current stage. `total` is `None` when the size is unknown.
    pub fn begin_transfer(&self, label: &str, total: Option<u64>) {
        let now = Instant::now();
        self.inner.lock().unwrap().transfer = Some(Transfer {
            label: label.to_owned(),
            done: 0,
            total,
            started: now,
            last_at: now,
            last_done: 0,
            ema_bps: None,
        });
    }

    /// The transfer is at `done` bytes in total.
    pub fn transfer_progress(&self, done: u64) {
        let mut s = self.inner.lock().unwrap();
        let Some(t) = s.transfer.as_mut() else { return };
        t.done = done;
        let now = Instant::now();
        let dt = now.duration_since(t.last_at);
        if dt >= SAMPLE_EVERY {
            let instant = done.saturating_sub(t.last_done) as f64 / dt.as_secs_f64();
            t.ema_bps = Some(smooth(t.ema_bps, instant));
            t.last_at = now;
            t.last_done = done;
        }
    }

    pub fn end_transfer(&self) {
        self.inner.lock().unwrap().transfer = None;
    }

    pub fn snapshot(&self) -> Snapshot {
        let s = self.inner.lock().unwrap();
        Snapshot {
            stage: s.stage.clone(),
            stage_no: s.stage_no,
            stage_of: s.stage_of,
            done: s.done,
            total: s.total,
            label: s.label.clone(),
            elapsed: s.started.elapsed(),
            transfer: s.transfer.as_ref().map(|t| {
                let elapsed = t.started.elapsed();
                TransferSnapshot {
                    label: t.label.clone(),
                    done: t.done,
                    total: t.total,
                    elapsed,
                    // Until the first sample lands, the average since the start.
                    bytes_per_sec: t.ema_bps.or_else(|| {
                        (elapsed.as_secs_f64() > 0.0).then(|| t.done as f64 / elapsed.as_secs_f64())
                    }),
                }
            }),
        }
    }
}

/// Exponential moving average of the speed: recent seconds count most, a single stall or burst does not.
pub fn smooth(previous: Option<f64>, sample: f64) -> f64 {
    match previous {
        Some(p) => 0.7 * p + 0.3 * sample,
        None => sample,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub stage: String,
    pub stage_no: usize,
    pub stage_of: usize,
    pub done: u64,
    pub total: u64,
    pub label: String,
    pub elapsed: Duration,
    pub transfer: Option<TransferSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransferSnapshot {
    pub label: String,
    pub done: u64,
    pub total: Option<u64>,
    pub elapsed: Duration,
    pub bytes_per_sec: Option<f64>,
}

impl Snapshot {
    /// `0.0..=1.0`, or `None` when the stage has no countable total.
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).clamp(0.0, 1.0))
    }

    /// Time left, by extrapolating the stage so far — shown only once it has run a moment and is not done.
    pub fn eta(&self) -> Option<Duration> {
        let f = self.fraction()?;
        (self.elapsed >= ETA_AFTER && f > 0.0 && f < 1.0)
            .then(|| self.elapsed.mul_f64(f64::from(1.0 - f) / f64::from(f)))
    }

    /// `412 / 1,142 · 36 % · about 12 s left` (or just the parts that are known).
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.total > 0 {
            parts.push(format!("{} / {}", group(self.done), group(self.total)));
        }
        if let Some(f) = self.fraction() {
            parts.push(format!("{} %", (f * 100.0).floor() as u32));
        }
        if let Some(eta) = self.eta() {
            parts.push(format!("about {} left", human_duration(eta)));
        }
        parts.join(" · ")
    }

    /// `Step 2 of 4 — Loading symbols`.
    pub fn title(&self) -> String {
        if self.stage_of > 1 {
            format!(
                "Step {} of {} — {}",
                self.stage_no, self.stage_of, self.stage
            )
        } else {
            self.stage.clone()
        }
    }
}

impl TransferSnapshot {
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }

    pub fn eta(&self) -> Option<Duration> {
        let (total, bps) = (self.total?, self.bytes_per_sec?);
        (self.elapsed >= Duration::from_secs(1) && bps > 1.0 && self.done < total)
            .then(|| Duration::from_secs_f64((total - self.done) as f64 / bps))
    }

    /// `412 MB of 1.1 GB · 39 % · 41 MB/s · about 16 s left`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        parts.push(match self.total {
            Some(total) => format!("{} of {}", human_bytes(self.done), human_bytes(total)),
            None => human_bytes(self.done),
        });
        if let Some(f) = self.fraction() {
            parts.push(format!("{} %", (f * 100.0).floor() as u32));
        }
        if let Some(bps) = self.bytes_per_sec.filter(|b| *b >= 1.0) {
            parts.push(format!("{}/s", human_bytes(bps as u64)));
        }
        if let Some(eta) = self.eta() {
            parts.push(format!("about {} left", human_duration(eta)));
        }
        parts.join(" · ")
    }
}

/// `1.1 GB`, `312 MB`, `4.5 KB` — decimal units, one decimal below 100.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, f64); 3] = [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)];
    for (unit, size) in UNITS {
        if bytes as f64 >= size {
            let value = bytes as f64 / size;
            return if value >= 100.0 {
                format!("{value:.0} {unit}")
            } else {
                format!("{value:.1} {unit}")
            };
        }
    }
    format!("{bytes} B")
}

/// `12 s`, `1 min 5 s`, `2 h 3 min` — rounded up to a second, never `0 s`.
pub fn human_duration(d: Duration) -> String {
    let secs = d.as_secs() + u64::from(d.subsec_nanos() > 0);
    let secs = secs.max(1);
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min {} s", secs / 60, secs % 60),
        _ => format!("{} h {} min", secs / 3600, (secs % 3600) / 60),
    }
}

/// `1,142`.
pub fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(done: u64, total: u64, elapsed_ms: u64) -> Snapshot {
        Snapshot {
            stage: "Loading symbols".into(),
            stage_no: 2,
            stage_of: 4,
            done,
            total,
            label: String::new(),
            elapsed: Duration::from_millis(elapsed_ms),
            transfer: None,
        }
    }

    #[test]
    fn a_stage_counts_and_clamps() {
        let p = Progress::new();
        p.begin_stage(1, 4, "Finding debug files", 3);
        assert_eq!(p.snapshot().fraction(), Some(0.0));
        p.advance(1);
        p.advance(5);
        assert_eq!(p.snapshot().done, 3, "never past the total");
        assert_eq!(p.snapshot().fraction(), Some(1.0));
        p.begin_stage(2, 4, "Loading symbols", 0);
        assert_eq!(p.snapshot().fraction(), None, "no total, no fraction");
        assert_eq!(p.snapshot().done, 0, "a new stage starts from zero");
    }

    #[test]
    fn extra_work_grows_the_total_and_finishing_fills_the_bar() {
        let p = Progress::new();
        p.begin_stage(1, 1, "Fetching", 3);
        p.advance(1);
        p.add_total(2);
        assert_eq!(p.snapshot().total, 5);
        p.finish_stage();
        assert_eq!(p.snapshot().fraction(), Some(1.0));
    }

    #[test]
    fn the_eta_needs_history_and_a_fraction() {
        assert_eq!(snap(1, 10, 500).eta(), None, "too early");
        assert_eq!(snap(0, 10, 5000).eta(), None, "nothing done yet");
        assert_eq!(snap(10, 10, 5000).eta(), None, "finished");
        assert_eq!(snap(0, 0, 5000).eta(), None, "no total");
        // 25 % in 3 s → 9 s left.
        assert_eq!(snap(25, 100, 3000).eta().map(|d| d.as_secs()), Some(9));
    }

    #[test]
    fn summaries_read_naturally() {
        assert_eq!(
            snap(412, 1142, 6000).summary(),
            "412 / 1,142 · 36 % · about 11 s left"
        );
        assert_eq!(snap(0, 0, 0).summary(), "");
        assert_eq!(snap(3, 8, 100).summary(), "3 / 8 · 37 %");
        assert_eq!(snap(3, 8, 100).title(), "Step 2 of 4 — Loading symbols");
    }

    #[test]
    fn a_transfer_shows_bytes_speed_and_eta() {
        let t = TransferSnapshot {
            label: "dtb-ke-ui".into(),
            done: 412_000_000,
            total: Some(1_061_046_214),
            elapsed: Duration::from_secs(10),
            bytes_per_sec: Some(41_000_000.0),
        };
        assert_eq!(t.fraction().map(|f| (f * 100.0) as u32), Some(38));
        assert_eq!(
            t.summary(),
            "412 MB of 1.1 GB · 38 % · 41.0 MB/s · about 16 s left"
        );
        let unknown = TransferSnapshot { total: None, ..t };
        assert_eq!(unknown.fraction(), None);
        assert_eq!(unknown.eta(), None);
        assert!(
            unknown.summary().starts_with("412 MB · 41.0 MB/s"),
            "{}",
            unknown.summary()
        );
    }

    #[test]
    fn the_speed_is_smoothed() {
        assert_eq!(smooth(None, 100.0), 100.0);
        let next = smooth(Some(100.0), 200.0);
        assert!((next - 130.0).abs() < 1e-9, "{next}");
    }

    #[test]
    fn a_transfer_lives_and_dies_inside_a_stage() {
        let p = Progress::new();
        p.begin_stage(1, 4, "Finding debug files", 5);
        p.begin_transfer("dtb-ke-ui", Some(1000));
        p.transfer_progress(250);
        let t = p.snapshot().transfer.expect("running");
        assert_eq!((t.done, t.total), (250, Some(1000)));
        assert_eq!(t.fraction(), Some(0.25));
        p.end_transfer();
        assert!(p.snapshot().transfer.is_none());
        p.begin_transfer("x", None);
        p.begin_stage(2, 4, "Loading symbols", 1);
        assert!(
            p.snapshot().transfer.is_none(),
            "a new stage drops a stale transfer"
        );
    }

    #[test]
    fn sizes_durations_and_counts_format() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(4_500), "4.5 KB");
        assert_eq!(human_bytes(312_000_000), "312 MB");
        assert_eq!(human_bytes(1_061_046_214), "1.1 GB");
        assert_eq!(human_duration(Duration::from_millis(200)), "1 s");
        assert_eq!(human_duration(Duration::from_secs(59)), "59 s");
        assert_eq!(human_duration(Duration::from_secs(65)), "1 min 5 s");
        assert_eq!(
            human_duration(Duration::from_secs(3 * 3600 + 120)),
            "3 h 2 min"
        );
        assert_eq!(group(7), "7");
        assert_eq!(group(1142), "1,142");
        assert_eq!(group(1_234_567), "1,234,567");
    }
}
