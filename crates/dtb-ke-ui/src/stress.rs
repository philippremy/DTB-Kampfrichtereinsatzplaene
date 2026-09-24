//! Perf stress test (`DTB_KE_STRESS=1`): seeds a large fixture into the
//! (isolated — run with `DTB_KE_DATA_DIR`) database, then drives the UI
//! frame-by-frame with no human input, logging per-phase frame statistics.
//! See `scripts/perf-stress.sh`, which wraps this for Instruments.
//!
//! `DTB_KE_STRESS_SECS` = total run length (default 24, split over 3 phases);
//! `DTB_KE_STRESS_KEEP=1` keeps the app open afterwards.

use std::{cell::RefCell, rc::Rc, time::Duration, time::Instant};

use chrono::{Duration as Days, NaiveDate, NaiveTime};
use dtb_ke_types::*;
use gpui_kit::{App, ScrollHandle, Window, point, px};
use log::info;
use uuid::Uuid;

use crate::app::{AppShell, main_window_handle};
use crate::store::AppStore;

const SETTLE: Duration = Duration::from_secs(3);
const COMPETITIONS: usize = 120;
const BIG_TABLES: usize = 30;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    DetailScroll,
    SidebarScroll,
    SelectChurn,
}

const PHASES: [(Phase, &str); 3] = [
    (Phase::DetailScroll, "detail-scroll"),
    (Phase::SidebarScroll, "sidebar-scroll"),
    (Phase::SelectChurn, "select-churn"),
];

fn fixture() -> (Vec<CompetitionDTO>, Uuid) {
    let n = std::cell::Cell::new(0usize);
    let name = || {
        n.set(n.get() + 1);
        format!("Kampfrichter Nummer {}", n.get())
    };
    let tables = |count: usize| -> Vec<JudgingTableDTO> {
        (0..count)
            .map(|i| {
                let kind = match i % 4 {
                    0 => JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(GymWheelSTLMTableDTO {
                        head: name(),
                        diff1: name(),
                        diff2: name(),
                        exec1: name(),
                        exec2: name(),
                        exec3: name(),
                        exec4: name(),
                        art1: name(),
                        art2: name(),
                        art3: name(),
                        art4: name(),
                    })),
                    1 => JudgingTableKindDTO::Gym(GymWheelTableDTO::STL(GymWheelSTLTableDTO {
                        head: name(),
                        diff1: name(),
                        exec1: name(),
                        ..Default::default()
                    })),
                    2 => JudgingTableKindDTO::Cyr(CyrWheelTableDTO::Artistic(
                        CyrWheelArtisticTableDTO {
                            head: name(),
                            art1: name(),
                            ..Default::default()
                        },
                    )),
                    _ => JudgingTableKindDTO::Gym(GymWheelTableDTO::SPI(GymWheelSPITableDTO {
                        head: name(),
                        ..Default::default()
                    })),
                };
                JudgingTableDTO {
                    label: format!("Gerät {}", i + 1),
                    kind,
                }
            })
            .collect()
    };

    let base = NaiveDate::from_ymd_opt(2021, 1, 1).unwrap();
    let mut out = Vec::new();
    let mut big = Uuid::nil();
    for i in 0..COMPETITIONS {
        let id = Uuid::new_v4();
        let is_big = i == 0;
        if is_big {
            big = id;
        }
        let count = if is_big { BIG_TABLES } else { 6 };
        out.push(CompetitionDTO {
            id,
            name: if is_big {
                "Stresstest Deutsche Meisterschaft".into()
            } else {
                format!("Stresstest Wettkampf {i}")
            },
            organization: OrganizationDTO::DTB,
            location: "Musterstadt".into(),
            date: base + Days::days(i as i64 * 13),
            meeting_times: MeetingTimeDTO::Unified(NaiveTime::from_hms_opt(8, 30, 0).unwrap()),
            responsible_persons: vec!["Erika Mustermann".into()],
            spare_judges: SpareJudgesDTO {
                qualification: (0..6).map(|_| name()).collect(),
                finale: (0..4).map(|_| name()).collect(),
            },
            judging_tables: JudgingTablesDTO {
                qualification: tables(count),
                finale: tables(count / 2),
            },
            additional_remarks: RichTextDTO::default(),
        });
    }
    (out, big)
}

struct Run {
    started: Instant,
    per_phase: Duration,
    store: gpui_kit::Entity<AppStore>,
    detail: (gpui_kit::EntityId, ScrollHandle),
    sidebar: (gpui_kit::EntityId, ScrollHandle),
    ids: Vec<Uuid>,
    phase_ix: usize,
    frames: Vec<f32>,
    last_frame: Instant,
    last_select: Instant,
    select_ix: usize,
    keep: bool,
    phase_started: Instant,
    phase_cpu: Duration,
}

pub fn start(cx: &mut App) {
    let secs: u64 = std::env::var("DTB_KE_STRESS_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    let keep = std::env::var_os("DTB_KE_STRESS_KEEP").is_some();
    // The counters walk the layout tree per call and skew frame times, so they're opt-in.
    gpui_kit::enable_layout_stats(std::env::var_os("DTB_KE_STRESS_LAYOUT_STATS").is_some());
    info!("stress: starting ({secs} s over {} phases)", PHASES.len());

    cx.spawn(async move |cx| {
        // Let the DB open and the window paint before seeding.
        cx.background_executor().timer(Duration::from_secs(1)).await;
        let Some(handle) = main_window_handle() else {
            log::error!("stress: no main window");
            return;
        };
        let (dtos, big) = fixture();
        let ids: Vec<Uuid> = dtos.iter().map(|d| d.id).collect();
        let targets = cx.update(|cx| {
            handle
                .update(cx, |shell: &mut AppShell, _, cx| shell.stress_targets(cx))
                .ok()
        });
        let Some((store, sidebar, detail)) = targets else { return };
        cx.update(|cx| store.update(cx, |s, cx| s.seed_many(dtos, big, cx)));
        cx.background_executor().timer(SETTLE).await;

        let run = Rc::new(RefCell::new(Run {
            started: Instant::now(),
            per_phase: Duration::from_secs((secs / PHASES.len() as u64).max(1)),
            store,
            detail,
            sidebar,
            ids,
            phase_ix: 0,
            frames: Vec::new(),
            last_frame: Instant::now(),
            last_select: Instant::now(),
            select_ix: 0,
            keep,
            phase_started: Instant::now(),
            phase_cpu: process_cpu_time(),
        }));
        info!("stress: phase {} begins", PHASES[0].1);
        cx.update(|cx| {
            handle
                .update(cx, |_, window, _| tick(run, window))
                .ok();
        });
    })
    .detach();
}

/// Triangle wave in 0..=1 with the given period.
fn tri(t: f32, period: f32) -> f32 {
    let x = (t / period).fract();
    if x < 0.5 { x * 2.0 } else { 2.0 - x * 2.0 }
}

// Like a real wheel event: move the offset and notify only the owning view
// (`window.refresh()` would also bypass the view cache).
fn drive(target: &(gpui_kit::EntityId, ScrollHandle), t: f32, period: f32, cx: &mut App) {
    let max = target.1.max_offset().y.abs();
    target.1.set_offset(point(px(0.0), -(max * tri(t, period))));
    cx.notify(target.0);
}

fn tick(run: Rc<RefCell<Run>>, window: &mut Window) {
    window.on_next_frame(move |window, cx| {
        {
            let mut guard = run.borrow_mut();
            let r = &mut *guard;
            let now = Instant::now();
            r.frames.push(now.duration_since(r.last_frame).as_secs_f32() * 1000.0);
            r.last_frame = now;

            let elapsed = now.duration_since(r.started);
            let phase_ix = (elapsed.as_secs_f32() / r.per_phase.as_secs_f32()) as usize;
            if phase_ix != r.phase_ix {
                report(r);
                r.phase_ix = phase_ix;
                if phase_ix >= PHASES.len() {
                    info!("stress: done");
                    if !r.keep {
                        cx.quit();
                    }
                    return;
                }
                info!("stress: phase {} begins", PHASES[phase_ix].1);
            }

            let t = elapsed.as_secs_f32();
            match PHASES[r.phase_ix].0 {
                Phase::DetailScroll => drive(&r.detail, t, 1.6, cx),
                Phase::SidebarScroll => drive(&r.sidebar, t, 1.2, cx),
                Phase::SelectChurn => {
                    if now.duration_since(r.last_select) > Duration::from_millis(300) {
                        r.last_select = now;
                        r.select_ix = (r.select_ix + 1) % r.ids.len().min(12);
                        let id = r.ids[r.select_ix];
                        let store = r.store.clone();
                        store.update(cx, |s, cx| s.select(id, cx));
                    }
                }
            }
        }
        tick(run, window);
    });
}

/// User + system CPU time of this process (all threads).
fn process_cpu_time() -> Duration {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid out-pointer for the duration of the call.
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

fn report(r: &mut Run) {
    let wall = r.phase_started.elapsed();
    let cpu = process_cpu_time().saturating_sub(r.phase_cpu);
    r.phase_started = Instant::now();
    r.phase_cpu = process_cpu_time();
    info!(
        "stress:   process CPU {:.0}% of one core over the phase",
        cpu.as_secs_f64() * 100.0 / wall.as_secs_f64().max(1e-9)
    );
    let mut f = std::mem::take(&mut r.frames);
    if f.len() < 2 {
        return;
    }
    f.sort_by(|a, b| a.total_cmp(b));
    let mean = f.iter().sum::<f32>() / f.len() as f32;
    let p95 = f[(f.len() as f32 * 0.95) as usize];
    let secs = r.per_phase.as_secs_f32();
    let mut calls = gpui_kit::take_layout_calls();
    calls.sort_by_key(|c| std::cmp::Reverse(c.0));
    let total: u64 = calls.iter().map(|c| c.0).sum();
    let top: Vec<String> = calls
        .iter()
        .take(8)
        .map(|(ns, n)| format!("{:.1}ms/{}n", *ns as f64 / 1e6, n))
        .collect();
    let hist = |lo: u32, hi: u32| {
        let sel: Vec<_> = calls.iter().filter(|c| c.1 >= lo && c.1 < hi).collect();
        format!(
            "{}:{} calls {:.0}%",
            lo,
            sel.len(),
            sel.iter().map(|c| c.0).sum::<u64>() as f64 * 100.0 / total.max(1) as f64
        )
    };
    info!(
        "stress:   top calls {} | by subtree size: {} · {} · {} · {}",
        top.join(" "),
        hist(0, 10),
        hist(10, 100),
        hist(100, 1000),
        hist(1000, u32::MAX)
    );
    let outcomes = gpui_kit::take_view_cache_stats();
    if outcomes.iter().any(|(_, n)| *n > 0) {
        info!(
            "stress:   view cache: {}",
            outcomes
                .iter()
                .filter(|(_, n)| *n > 0)
                .map(|(name, n)| format!("{name} {n}"))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    let (hits, misses) = gpui_kit::take_taffy_cache_stats();
    info!(
        "stress:   taffy cache: {:.0} hits/draw, {:.0} misses/draw",
        hits as f64 / f.len() as f64,
        misses as f64 / f.len() as f64
    );
    let (created, reused) = gpui_kit::take_reuse_stats();
    info!(
        "stress:   layout nodes: {created} created, {reused} reused ({:.0}% reused)",
        reused as f64 * 100.0 / (created + reused).max(1) as f64
    );
    let [frames, nodes, layout_ns, m_calls, m_ns] = gpui_kit::take_layout_stats();
    let fr = frames.max(1) as f64;
    info!(
        "stress:   {} compute_layout calls over {} draws ({:.1}/draw), {:.1} ms total per draw",
        frames,
        f.len(),
        frames as f64 / f.len() as f64,
        layout_ns as f64 / f.len() as f64 / 1e6,
    );
    info!(
        "stress:   layout: {:.0} nodes/frame · {:.2} ms/frame compute_layout · {:.0} measure calls/frame ({:.2} ms, text etc.)",
        nodes as f64 / fr,
        layout_ns as f64 / fr / 1e6,
        m_calls as f64 / fr,
        m_ns as f64 / fr / 1e6,
    );
    info!(
        "stress: {:<14} {:>5} frames ({:>5.1} fps) · mean {:>6.2} ms · p95 {:>6.2} ms · max {:>7.2} ms",
        PHASES[r.phase_ix.min(PHASES.len() - 1)].1,
        f.len(),
        f.len() as f32 / secs,
        mean,
        p95,
        f[f.len() - 1],
    );
}
