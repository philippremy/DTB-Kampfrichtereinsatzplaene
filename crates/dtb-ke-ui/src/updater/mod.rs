//! In-app updater.
//!
//! Releases live on Codeberg (Forgejo), which none of `self_update`'s forge
//! backends target directly, so we use its **`manifest` backend**: CI publishes
//! a small `manifest.json` at a stable URL (a rolling `latest` release tag) plus
//! the release archives beside it, each signed with **zipsign / ed25519**. The
//! app embeds the *public* verification key (`assets/release.pub` →
//! `build.rs::emit_update_key`); with no key the whole feature is off
//! ([`available`] is `false`, like `mail::available()`).
//!
//! The **check** parses the manifest ourselves (`ureq`, re-exported by
//! `self_update`) so the toast can show version / date / size and honour
//! "skipped" versions. The **install** hands off to `self_update`'s
//! `manifest::Update`, which re-fetches, downloads the target-matched archive,
//! verifies the signature + digest, and swaps the macOS `.app` bundle / Windows
//! folder as one unit. Linux (`.deb` / `.rpm` / AppImage) can't be swapped
//! safely from in-process, so there the toast just opens the release page.
//!
//! Everything network / filesystem runs on `cx.background_executor()`.

mod toast;

pub use toast::UpdaterToast;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::{Context, EventEmitter, Task};
use serde::Deserialize;

use crate::build_info;
use crate::settings::{Settings, UpdateChannel};

mod key {
    include!(concat!(env!("OUT_DIR"), "/update_key.rs"));
}

/// The shipped bundle/executable name inside a downloaded update archive —
/// matches `dtb-ke-bundle::meta::DISPLAY_NAME` (**not** the raw cargo
/// `[[bin]]` name in Cargo.toml, which is kebab-case and only ever used to
/// locate the build artifact before packaging — see that Cargo.toml's
/// comment). dtb-ke-ui can't depend on dtb-ke-bundle, so this has to be
/// hand-kept in sync with it.
const BIN_NAME: &str = "DTB Kampfrichtereinsatzpläne";

/// Where each release channel's manifest lives — a rolling tag per channel
/// (`latest` for tagged releases, `tip` re-published on every commit to
/// `main`) keeps the URL stable. See `UPDATER.md`. Override with
/// `DTB_KE_UPDATE_MANIFEST` (testing) — bypasses the channel entirely.
const MANIFEST_URL_STABLE: &str = "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/releases/download/latest/manifest.json";
const MANIFEST_URL_TIP: &str = "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/releases/download/tip/manifest.json";

/// Delay after launch before the first automatic check (let the app settle).
pub const STARTUP_DELAY: Duration = Duration::from_secs(4);
/// Minimum spacing between automatic checks while the app stays open.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Whether the updater is usable in this build (a verification key is embedded).
pub fn available() -> bool {
    key::VERIFY_KEY.is_some()
}

/// Whether this platform can install an update in place on `channel`.
/// Elsewhere the toast opens the download page instead.
///
/// Tip builds are ad-hoc signed only (no per-commit notarization — see
/// `UPDATER.md`), so a self-swapped `.app` does hit Gatekeeper friction on
/// relaunch (a first-run "unidentified developer" prompt) — accepted
/// deliberately: this is a small-user-base app where ad-hoc-everywhere beats
/// making people leave the app to reinstall by hand every Tip build. Windows
/// has no equivalent re-check on an in-place swap by an already-running
/// process, so it always self-installs regardless of channel.
pub fn can_self_install(_channel: UpdateChannel) -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

fn manifest_url(channel: UpdateChannel) -> String {
    if let Ok(url) = std::env::var("DTB_KE_UPDATE_MANIFEST") {
        return url;
    }
    match channel {
        UpdateChannel::Stable => MANIFEST_URL_STABLE.to_owned(),
        UpdateChannel::Tip => MANIFEST_URL_TIP.to_owned(),
    }
}

/// A release newer than the running build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Bare semver, e.g. `"0.2.0"`.
    pub version: String,
    /// Human date for the card (`"4. Sep 2026"`), or empty.
    pub date: String,
    /// The release-notes / changelog URL, if the manifest carries one.
    pub notes_url: Option<String>,
    /// Size of this platform's archive in bytes, if the manifest carries it.
    pub size: Option<u64>,
}

/// What the updater is doing right now.
#[derive(Clone, Debug)]
pub enum State {
    /// Nothing found / not checked yet.
    Idle,
    /// A check is in flight.
    Checking,
    /// A manual check found nothing (shown briefly, then back to `Idle`).
    UpToDate,
    /// An update is available and the toast is showing.
    Available(Release),
    /// The archive is downloading / installing. `progress` is the download
    /// fraction (`0.0..=1.0`), `None` until the total size is known and again
    /// once the download finishes and the (progress-less) verify + swap runs.
    Installing {
        release: Release,
        progress: Option<f32>,
    },
    /// Installed — the app needs to relaunch to finish.
    Restart(Release),
    /// The last check or install failed.
    Failed(String),
}

/// Emitted so [`AppShell`](crate::app::AppShell) can react (rebuild the menu,
/// flush + relaunch).
#[derive(Clone, Copy, Debug)]
pub enum UpdaterEvent {
    /// The state changed in a way the shell should re-render / re-sync menus for.
    Changed,
    /// The install finished; the shell should flush documents and relaunch.
    RelaunchRequested,
}

pub struct Updater {
    state: State,
    /// Toast expanded into the full card (vs. the collapsed pill).
    expanded: bool,
    /// Dismissed for this session ("Später"); a later check re-shows it.
    snoozed: bool,
    /// When the last automatic check ran (monotonic-ish; `Instant`).
    last_check: Option<std::time::Instant>,
    _task: Option<Task<()>>,
}

impl Updater {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            expanded: false,
            snoozed: false,
            last_check: None,
            _task: None,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }

    /// Whether the toast should be on screen right now.
    pub fn toast_visible(&self) -> bool {
        !self.snoozed
            && matches!(
                self.state,
                State::UpToDate
                    | State::Available(_)
                    | State::Installing { .. }
                    | State::Restart(_)
                    | State::Failed(_)
            )
    }

    /// The version the toast is about, for the menu / logs.
    pub fn pending_version(&self) -> Option<&str> {
        match &self.state {
            State::Available(r) | State::Restart(r) | State::Installing { release: r, .. } => {
                Some(&r.version)
            }
            _ => None,
        }
    }

    pub fn toggle_expanded(&mut self, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        cx.notify();
        cx.emit(UpdaterEvent::Changed);
    }

    /// Collapse the expanded card back to the pill — a click outside the card.
    /// The pill itself stays; unlike [`Self::dismiss`] / [`Self::snooze`], the
    /// state (`Available`, `Failed`, …) is untouched.
    pub fn collapse(&mut self, cx: &mut Context<Self>) {
        if self.expanded {
            self.expanded = false;
            cx.notify();
            cx.emit(UpdaterEvent::Changed);
        }
    }

    /// "Später" — hide the toast until the next check finds something.
    pub fn snooze(&mut self, cx: &mut Context<Self>) {
        self.snoozed = true;
        self.expanded = false;
        cx.notify();
        cx.emit(UpdaterEvent::Changed);
    }

    /// "Überspringen" — remember this version and don't nag again until a newer
    /// one appears.
    pub fn skip(&mut self, cx: &mut Context<Self>) {
        if let Some(version) = self.pending_version().map(str::to_owned) {
            log::info!("update {version} skipped by the user");
            Settings::update(cx, |s| s.skipped_update = Some(version));
        }
        self.state = State::Idle;
        self.snoozed = false;
        self.expanded = false;
        cx.notify();
        cx.emit(UpdaterEvent::Changed);
    }

    /// Run a check now unless one is already in flight. `manual` bypasses the
    /// "skipped version" filter and the recheck interval, and reports
    /// `UpToDate` visibly.
    pub fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if !available() {
            return;
        }
        if matches!(
            self.state,
            State::Checking | State::Installing { .. } | State::Restart(_)
        ) {
            return;
        }
        if !manual {
            if !Settings::global(cx).auto_update {
                return;
            }
            // Don't re-check while a result is already on screen or was just
            // fetched — the user hasn't acted on it yet.
            if matches!(
                self.state,
                State::Available(_) | State::Failed(_) | State::UpToDate
            ) {
                return;
            }
            if let Some(last) = self.last_check
                && last.elapsed() < RECHECK_INTERVAL
            {
                return;
            }
        }

        self.last_check = Some(std::time::Instant::now());
        self.state = State::Checking;
        self.snoozed = false;
        cx.notify();
        cx.emit(UpdaterEvent::Changed);

        let settings = Settings::global(cx);
        let skipped = if manual {
            None
        } else {
            settings.skipped_update
        };
        let url = manifest_url(settings.update_channel);

        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { fetch_newest(&url, skipped.as_deref()) })
                .await;

            this.update(cx, |this, cx| {
                match result {
                    Ok(Some(release)) => {
                        log::info!("update available: {} ({})", release.version, release.date);
                        this.state = State::Available(release);
                    }
                    Ok(None) => {
                        log::debug!("update check: up to date");
                        this.state = if manual { State::UpToDate } else { State::Idle };
                    }
                    Err(err) => {
                        log::warn!("update check failed: {err}");
                        this.state = if manual {
                            State::Failed(err)
                        } else {
                            State::Idle
                        };
                    }
                }
                cx.notify();
                cx.emit(UpdaterEvent::Changed);
                if manual && matches!(this.state, State::UpToDate | State::Failed(_)) {
                    this.auto_dismiss(cx);
                }
            })
            .ok();
        }));
    }

    /// Clear a transient `UpToDate` / `Failed` toast after a short delay.
    fn auto_dismiss(&mut self, cx: &mut Context<Self>) {
        self._task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(6)).await;
            this.update(cx, |this, cx| {
                if matches!(this.state, State::UpToDate | State::Failed(_)) {
                    this.state = State::Idle;
                    this.expanded = false;
                    cx.notify();
                    cx.emit(UpdaterEvent::Changed);
                }
            })
            .ok();
        }));
    }

    /// "Installieren & neu starten" — download, verify, swap, then ask the shell
    /// to flush + relaunch. Only valid from [`State::Available`] on a platform
    /// where [`can_self_install`] holds.
    pub fn install(&mut self, cx: &mut Context<Self>) {
        let State::Available(release) = &self.state else {
            return;
        };
        let release = release.clone();
        let channel = Settings::global(cx).update_channel;
        if !can_self_install(channel) {
            return;
        }
        log::info!("installing update {} ({channel:?})", release.version);
        self.state = State::Installing {
            release: release.clone(),
            progress: None,
        };
        self.expanded = true;
        cx.notify();
        cx.emit(UpdaterEvent::Changed);

        let url = manifest_url(channel);
        self._task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let progress = Arc::new(DownloadProgress::default());
            let install = {
                let progress = progress.clone();
                bg.spawn(async move { run_install(&url, &progress) })
            };
            let mut install = std::pin::pin!(install);

            // Poll the install task; between polls, push the download fraction
            // into `State::Installing` so the toast can draw a bar.
            let result = loop {
                let tick = std::pin::pin!(bg.timer(Duration::from_millis(120)));
                match futures::future::select(install.as_mut(), tick).await {
                    futures::future::Either::Left((result, _)) => break result,
                    futures::future::Either::Right(((), _)) => {
                        let frac = progress.fraction();
                        if this
                            .update(cx, |this, cx| {
                                if let State::Installing { progress: p, .. } = &mut this.state
                                    && *p != frac
                                {
                                    *p = frac;
                                    cx.notify();
                                }
                            })
                            .is_err()
                        {
                            return; // the Updater entity is gone
                        }
                    }
                }
            };

            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    log::info!("update installed — relaunch pending");
                    this.state = State::Restart(release);
                    cx.notify();
                    cx.emit(UpdaterEvent::Changed);
                    cx.emit(UpdaterEvent::RelaunchRequested);
                }
                Err(err) => {
                    log::error!("update install failed: {err}");
                    this.state = State::Failed(err);
                    cx.notify();
                    cx.emit(UpdaterEvent::Changed);
                }
            })
            .ok();
        }));
    }

    /// The toast's primary button: install (self-install platforms), open the
    /// download page (elsewhere), or relaunch (after a completed install).
    pub fn primary_action(&mut self, cx: &mut Context<Self>) {
        let channel = Settings::global(cx).update_channel;
        match &self.state {
            State::Available(_) if can_self_install(channel) => self.install(cx),
            State::Available(_) => {
                let url = self.release_page();
                cx.open_url(&url);
            }
            State::Restart(_) => cx.emit(UpdaterEvent::RelaunchRequested),
            _ => {}
        }
    }

    /// Close a transient (`Failed` / `UpToDate`) toast.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.state = State::Idle;
        self.expanded = false;
        self.snoozed = false;
        cx.notify();
        cx.emit(UpdaterEvent::Changed);
    }

    /// The URL the "Herunterladen" / "Änderungen anzeigen" affordances open.
    pub fn release_page(&self) -> String {
        match &self.state {
            State::Available(r) | State::Restart(r) | State::Installing { release: r, .. } => r
                .notes_url
                .clone()
                .unwrap_or_else(|| crate::actions::REPOSITORY_URL.to_owned()),
            _ => crate::actions::REPOSITORY_URL.to_owned(),
        }
    }
}

impl EventEmitter<UpdaterEvent> for Updater {}

// ── the blocking self_update / http work (background executor only) ───────

/// Rank a release-asset file name for the platform this build runs on — lower
/// is better, `None` means "not a candidate".
///
/// `0` / `1` mirror `self_update`'s own native matching (the full target
/// triple, then the arch + os tokens). `2` — **macOS only** — accepts a
/// `universal-apple-darwin` (fat) archive as a deliberate last resort: an
/// arch-specific slice is always preferred, but the app can still update from
/// the fat archive while the macOS CI artifacts are universal-only (and after
/// they stop being, if an old universal release is all that is left).
fn asset_priority(name: &str) -> Option<u32> {
    let target = self_update::get_target();
    if name.contains(target) {
        return Some(0);
    }
    let arch = target.split('-').next().filter(|s| !s.is_empty());
    let os = [
        "linux", "darwin", "windows", "freebsd", "netbsd", "openbsd", "android", "ios", "wasm",
    ]
    .into_iter()
    .find(|os| target.contains(os));
    if let (Some(arch), Some(os)) = (arch, os)
        && name.contains(arch)
        && name.contains(os)
    {
        return Some(1);
    }
    if cfg!(target_os = "macos") && name.contains("universal-apple-darwin") {
        return Some(2);
    }
    None
}

/// Fetch + parse the manifest at `url`, return the newest release strictly
/// newer than the running build (and newer than `skipped`, if given).
fn fetch_newest(url: &str, skipped: Option<&str>) -> Result<Option<Release>, String> {
    let body = self_update::ureq::get(url)
        .call()
        .map_err(|e| format!("Manifest nicht erreichbar: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Manifest unlesbar: {e}"))?;

    let manifest: Manifest =
        serde_json::from_str(&body).map_err(|e| format!("Manifest ungültig: {e}"))?;
    if manifest.schema != 1 {
        return Err(format!(
            "Manifest-Schema {} wird nicht unterstützt",
            manifest.schema
        ));
    }

    let current = build_info::APP_VERSION;

    let mut best: Option<Release> = None;
    for entry in manifest.releases {
        // Only real semver releases newer than what we run.
        let newer = self_update::version::bump_is_greater(current, &entry.version).unwrap_or(false);
        if !newer {
            continue;
        }
        if let Some(skipped) = skipped {
            // `entry` must be strictly newer than the skipped version to count.
            if !self_update::version::bump_is_greater(skipped, &entry.version).unwrap_or(false) {
                continue;
            }
        }
        // Same native-first / universal-fallback selection `run_install`'s
        // `asset_matcher` uses, so the size shown matches the archive that
        // will actually be downloaded.
        let size = entry
            .assets
            .iter()
            .filter_map(|a| asset_priority(&a.name).map(|prio| (prio, a)))
            .min_by_key(|&(prio, _)| prio)
            .and_then(|(_, a)| a.size);

        // `bump_is_greater(a, b)` == "b is newer than a": keep `entry` only if
        // it's newer than the best seen so far.
        let supersedes_best = best.as_ref().is_none_or(|b| {
            self_update::version::bump_is_greater(&b.version, &entry.version).unwrap_or(false)
        });
        if !supersedes_best {
            continue;
        }

        best = Some(Release {
            version: entry.version.clone(),
            date: entry.date.as_deref().map(format_date).unwrap_or_default(),
            notes_url: entry.notes_url.clone(),
            size,
        });
    }
    Ok(best)
}

/// Shared download counters — written by `self_update`'s progress callback on
/// its download thread, read by the install task's poll loop.
#[derive(Default)]
struct DownloadProgress {
    downloaded: AtomicU64,
    /// Bytes; `0` until the server's `Content-Length` is seen.
    total: AtomicU64,
}

impl DownloadProgress {
    fn set(&self, downloaded: u64, total: Option<u64>) {
        self.downloaded.store(downloaded, Ordering::Relaxed);
        if let Some(total) = total {
            self.total.store(total, Ordering::Relaxed);
        }
    }

    /// Download fraction in `0.0..=1.0`, `None` until a non-zero total is known.
    fn fraction(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| {
            let done = self.downloaded.load(Ordering::Relaxed).min(total);
            (done as f64 / total as f64) as f32
        })
    }
}

/// Download the newest release's archive, verify it, and swap it in.
fn run_install(manifest_url: &str, progress: &Arc<DownloadProgress>) -> Result<(), String> {
    let key = key::VERIFY_KEY.ok_or("kein Prüfschlüssel in dieser Programmversion")?;

    let progress = progress.clone();
    let mut builder = self_update::backends::manifest::Update::configure();
    builder
        .manifest_url(manifest_url)
        .bin_name(BIN_NAME)
        .current_version(build_info::APP_VERSION)
        .no_confirm(true)
        .show_output(false)
        .show_download_progress(false)
        // Both channels' archives are zipsign-signed (Tip too — see
        // `.forgejo/workflows/tip.yml`) and digest-pinned in the manifest.
        .verify_release_digest(true)
        .verifying_keys([key])
        .progress_callback(move |downloaded, total| progress.set(downloaded, total))
        // Replace self_update's built-in target-substring match: prefer a
        // native, arch-specific archive exactly as it would, but allow a
        // `universal-apple-darwin` fat archive as a last resort on macOS
        // (`asset_priority`). The chosen asset keeps its manifest digest, so
        // `verify_release_digest` still applies. See UPDATER.md.
        .asset_matcher(|assets| {
            assets
                .iter()
                .filter_map(|a| asset_priority(a.name()).map(|prio| (prio, a)))
                .min_by_key(|&(prio, _)| prio)
                .map(|(_, a)| a.clone())
        });

    #[cfg(target_os = "macos")]
    builder.bundle_path_in_archive(format!("{BIN_NAME}.app"));

    #[cfg(target_os = "windows")]
    {
        builder.bundle_path_in_archive(BIN_NAME);
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                builder.bundle_install_path(dir);
            }
        }
    }

    let update = builder.build().map_err(|e| e.to_string())?;
    match update.update() {
        Ok(status) => {
            log::info!("self_update: {status}");
            Ok(())
        }
        Err(err) => Err(friendly_install_error(err)),
    }
}

/// Turn a `self_update::Error` into a German sentence for the toast.
fn friendly_install_error(err: self_update::Error) -> String {
    use self_update::Error;
    match err {
        Error::InstallPathNotWritable { .. } => {
            "Das Programmverzeichnis ist schreibgeschützt. Bitte die neue Version manuell \
             herunterladen."
                .to_owned()
        }
        Error::VerificationRejected { .. } | Error::ArchiveVerificationRejected { .. } => {
            "Die Signatur der heruntergeladenen Datei ist ungültig — die Aktualisierung wurde \
             abgebrochen."
                .to_owned()
        }
        Error::ChecksumMismatch { .. } => {
            "Die Prüfsumme der heruntergeladenen Datei stimmt nicht — die Aktualisierung wurde \
             abgebrochen."
                .to_owned()
        }
        other => format!("Die Aktualisierung ist fehlgeschlagen: {other}"),
    }
}

/// `"2026-09-04T00:00:00Z"` / `"2026-09-04"` → `"4. Sep 2026"`; passthrough on
/// anything unexpected.
fn format_date(raw: &str) -> String {
    let date_part = raw.split(['T', ' ']).next().unwrap_or(raw);
    match chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d") {
        Ok(d) => d.format("%-d. %b %Y").to_string(),
        Err(_) => raw.to_owned(),
    }
}

// ── our (richer) view of the same manifest self_update parses ─────────────

#[derive(Deserialize)]
struct Manifest {
    schema: u64,
    #[serde(default)]
    releases: Vec<ManifestRelease>,
}

#[derive(Deserialize)]
struct ManifestRelease {
    version: String,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    notes_url: Option<String>,
    #[serde(default)]
    assets: Vec<ManifestAsset>,
}

#[derive(Deserialize)]
struct ManifestAsset {
    name: String,
    #[serde(default)]
    size: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_date_is_german_short() {
        assert_eq!(format_date("2026-09-04T00:00:00Z"), "4. Sep 2026");
        assert_eq!(format_date("2026-12-24"), "24. Dec 2026");
        assert_eq!(format_date("garbage"), "garbage");
    }

    #[test]
    fn newest_release_is_picked_and_old_ones_ignored() {
        let body = r#"{
            "schema": 1,
            "releases": [
                { "version": "0.1.0", "assets": [] },
                { "version": "0.3.0", "date": "2026-09-04",
                  "assets": [{ "name": "x-0.3.0-nope.tar.gz", "size": 100 }] },
                { "version": "0.2.0", "assets": [] }
            ]
        }"#;
        let manifest: Manifest = serde_json::from_str(body).unwrap();
        // Mirror fetch_newest's core loop against a fixed "current".
        let current = "0.1.5";
        let mut best: Option<String> = None;
        for e in manifest.releases {
            if !self_update::version::bump_is_greater(current, &e.version).unwrap_or(false) {
                continue;
            }
            let supersedes = best.as_ref().is_none_or(|b| {
                self_update::version::bump_is_greater(b, &e.version).unwrap_or(false)
            });
            if supersedes {
                best = Some(e.version);
            }
        }
        assert_eq!(best.as_deref(), Some("0.3.0"));
    }

    #[test]
    fn asset_priority_prefers_native_then_universal() {
        let t = self_update::get_target();
        let native = format!("App-1.0.0-{t}.tar.gz");
        let universal = "App-1.0.0-universal-apple-darwin.tar.gz";
        let unrelated = "App-1.0.0-mips64-unknown-none.tar.gz";

        assert_eq!(asset_priority(&native), Some(0));
        assert_eq!(asset_priority(unrelated), None);

        // Native wins even when the universal archive is listed first.
        let names = [universal.to_string(), native.clone()];
        let best = names
            .iter()
            .filter_map(|n| asset_priority(n).map(|p| (p, n.as_str())))
            .min_by_key(|&(p, _)| p)
            .map(|(_, n)| n);
        assert_eq!(best, Some(native.as_str()));

        // The universal archive is only a candidate on macOS.
        #[cfg(target_os = "macos")]
        assert_eq!(asset_priority(universal), Some(2));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(asset_priority(universal), None);
    }
}
