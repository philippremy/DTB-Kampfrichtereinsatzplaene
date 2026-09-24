//! Backend state layer.
//!
//! [`AppStore`] is the root entity: it owns the database handle, the sidebar
//! list (`year` + `name` per competition, read straight from dedicated columns),
//! the current selection, and the set of open [`CompetitionDocument`]s.
//!
//! Everything is asynchronous and non-blocking: the `turso::Database` is opened
//! once on the foreground executor at startup and then stays on the main
//! thread; every individual query runs on `cx.background_executor()` via a
//! cloned `turso::Connection`. Writes are debounced per document (see
//! [`document`]); reads happen on selection.

mod document;

// The editor sub-entities are the UI layer's entry points; not consumed yet.
#[allow(unused_imports)]
pub use document::{
    CompetitionDocument, DocumentEvent, JudgingTableEditor, MetadataEditor, RemarksEditor,
    RoundEditor, SaveState,
};

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{Local, NaiveTime, Utc};
use dtb_ke_persist::{self as persist, CompetitionSummary, Db, TrashedCompetition};
use dtb_ke_types::{CompetitionDTO, MeetingTimeDTO, OrganizationDTO};
use gpui_kit::{App, AppContext, AsyncApp, Context, Entity, Subscription, Task, WeakEntity};
use log::{debug, error, info, trace, warn};
use uuid::Uuid;

use crate::settings::Settings;

use crate::i18n::ActiveLocale;

use crate::filesystem::FilesystemHelper;
use crate::model::{Competition, CompetitionMeta, RichText, Round};

/// `AppShell`'s backup-loop startup delay — after the DB is bootstrapped and
/// the first autosave cycle has had time to settle (looser than the
/// updater's own 4s `STARTUP_DELAY`, which has no such dependency).
pub const BACKUP_STARTUP_DELAY: Duration = Duration::from_secs(10);
/// `AppShell`'s backup-loop recheck interval — same as the updater's own
/// `RECHECK_INTERVAL`; a long-running session still catches a midnight
/// rollover before the day is out.
pub const BACKUP_RECHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Connection / load status of the store as a whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Ready,
    Failed(String),
}

/// Root backend entity. Construct once with [`AppStore::new`] and keep the
/// returned handle for the lifetime of the app.
pub struct AppStore {
    db: Option<Db>,
    summaries: Vec<CompetitionSummary>,
    /// Soft-deleted competitions — the trash window's own listing. Kept in
    /// memory the same way `summaries` is (loaded at bootstrap, updated
    /// optimistically by every mutating method), so the trash window just
    /// observes this entity rather than issuing its own queries.
    trashed: Vec<TrashedCompetition>,
    selected: Option<Uuid>,
    open: HashMap<Uuid, Entity<CompetitionDocument>>,
    /// Observers on open documents that keep the sidebar summary (name / year)
    /// and [`Self::any_dirty`] in step with in-progress edits.
    doc_subs: HashMap<Uuid, Subscription>,
    /// Whether any open document has changes not yet flushed to the database.
    /// Drives the macOS window "edited" dot (see `AppShell`).
    any_dirty: bool,
    /// `(can_undo, can_redo)` for the *selected* document — drives the Edit
    /// menu. Recomputed on selection change and on any open document's edit.
    undo_state: (bool, bool),
    status: Status,
    /// Held so the startup task is not cancelled.
    _init: Task<()>,
}

impl AppStore {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let init = cx.spawn(async move |this, cx| Self::bootstrap(this, cx).await);
            Self {
                db: None,
                summaries: Vec::new(),
                trashed: Vec::new(),
                selected: None,
                open: HashMap::new(),
                doc_subs: HashMap::new(),
                any_dirty: false,
                undo_state: (false, false),
                status: Status::Connecting,
                _init: init,
            }
        })
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Whether any open document has unsaved changes.
    pub fn any_document_unsaved(&self) -> bool {
        self.any_dirty
    }

    /// `(can_undo, can_redo)` for the currently selected document.
    pub fn selected_undo_state(&self) -> (bool, bool) {
        self.undo_state
    }

    /// Force-flush every open document; the returned task resolves once all
    /// writes complete. Used before the updater relaunches the app.
    pub fn flush_all(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let docs: Vec<_> = self.open.values().cloned().collect();
        let flushes: Vec<Task<()>> = docs
            .into_iter()
            .map(|doc| doc.update(cx, |doc, cx| doc.flush_now(cx)))
            .collect();
        cx.spawn(async move |_, _| {
            for flush in flushes {
                flush.await;
            }
        })
    }

    /// Undo / redo the selected document, if any.
    pub fn undo_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(doc) = self.selected_document().cloned() {
            doc.update(cx, |doc, cx| doc.undo(cx));
        }
    }

    pub fn redo_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(doc) = self.selected_document().cloned() {
            doc.update(cx, |doc, cx| doc.redo(cx));
        }
    }

    /// The sidebar list: newest year first, then by name. Kept sorted.
    pub fn summaries(&self) -> &[CompetitionSummary] {
        &self.summaries
    }

    /// The trash window's list: most recently deleted first.
    pub fn trashed(&self) -> &[TrashedCompetition] {
        &self.trashed
    }

    pub fn selected_id(&self) -> Option<Uuid> {
        self.selected
    }

    /// Whether a competition with this id is in the sidebar list.
    pub fn contains(&self, id: Uuid) -> bool {
        self.summaries.iter().any(|s| s.id == id)
    }

    /// The display name of a listed competition.
    pub fn name_of(&self, id: Uuid) -> Option<&str> {
        self.summaries
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.as_str())
    }

    pub fn selected_document(&self) -> Option<&Entity<CompetitionDocument>> {
        self.selected.and_then(|id| self.open.get(&id))
    }

    pub fn document(&self, id: Uuid) -> Option<&Entity<CompetitionDocument>> {
        self.open.get(&id)
    }

    /// Select a competition, loading and opening its document if it is not
    /// already open. Flushes the previously selected document first.
    pub fn select(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.selected == Some(id) {
            return;
        }
        trace!("select {id}");

        if let Some(previous) = self.selected.and_then(|prev| self.open.get(&prev)).cloned() {
            debug!(
                "flushing previously selected {:?} before switch",
                self.selected
            );
            previous.update(cx, |doc, cx| doc.flush_now(cx).detach());
        }

        if self.open.contains_key(&id) {
            debug!("selecting already-open {id}");
            self.selected = Some(id);
            self.recompute_undo_state(cx);
            cx.notify();
            return;
        }

        let Some(db) = self.db.as_ref() else {
            warn!("select({id}) ignored — database not ready");
            return;
        };
        let connection = db.connection();
        debug!("loading competition {id} from the database");

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let loaded = background
                .spawn(async move { persist::load_competition(&connection, id).await })
                .await;

            this.update(cx, |this, cx| {
                match loaded {
                    Ok(Some(dto)) => {
                        if let Some(db) = this.db.as_ref() {
                            let connection = db.connection();
                            let competition = Competition::from(dto);
                            let doc =
                                cx.new(|cx| CompetitionDocument::new(competition, connection, cx));
                            this.track_document(id, &doc, cx);
                            this.open.insert(id, doc);
                            this.selected = Some(id);
                            this.recompute_undo_state(cx);
                            info!(
                                "opened competition {id} (\"{}\")",
                                this.name_of(id).unwrap_or("?")
                            );
                        }
                    }
                    Ok(None) => {
                        // Competition vanished between listing and opening; drop it.
                        warn!(
                            "competition {id} vanished between listing and opening — dropping it"
                        );
                        this.summaries.retain(|s| s.id != id);
                    }
                    Err(err) => {
                        error!("loading competition {id} failed: {err}");
                        this.status = Status::Failed(err.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// As [`Self::select`], but also invokes `then` once `id` is the active
    /// selection — synchronously if it was already open, or after the
    /// background load lands otherwise. Used by call sites that select and
    /// immediately act on a competition that might not be open yet (e.g. the
    /// sidebar context menu's "Export …" / "Wettkampfeinstellungen …" /
    /// "Vorschau" items, which reuse `AppShell`'s existing handlers — those
    /// assume the target is already the selection).
    pub fn select_then(
        &mut self,
        id: Uuid,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut App) + 'static,
    ) {
        if self.selected == Some(id) {
            then(cx);
            return;
        }
        let was_open = self.open.contains_key(&id);
        self.select(id, cx);
        if was_open {
            then(cx);
            return;
        }

        // `select` just kicked off an async load; poll until it lands (or the
        // competition turns out to be gone) rather than duplicating its
        // loading logic here — this is a rare, user-triggered path, not a
        // hot one.
        cx.spawn(async move |this, cx| {
            loop {
                let state = this.read_with(cx, |this, _| (this.selected == Some(id), this.contains(id)));
                let (selected, listed) = match state {
                    Ok(state) => state,
                    Err(_) => return,
                };
                if selected {
                    break;
                }
                if !listed {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(15))
                    .await;
            }
            cx.update(|cx| then(cx));
        })
        .detach();
    }

    /// Persist a new competition (its metadata filled in via the "new
    /// competition" dialog) and open it. The judging tables / remarks start
    /// empty.
    pub fn create_with_meta(&mut self, meta: CompetitionMeta, cx: &mut Context<Self>) {
        let Some(db) = self.db.as_ref() else {
            warn!("create_with_meta ignored — database not ready");
            return;
        };
        let connection = db.connection();

        let competition = Competition {
            id: Uuid::new_v4(),
            meta,
            qualification: Round::default(),
            finale: Round::default(),
            remarks: RichText::default(),
        };
        let id = competition.id;
        let date = competition.meta.date;
        let name = competition.meta.name.clone();
        let dto = CompetitionDTO::from(&competition);
        debug!("creating competition {id} (\"{name}\", {date})");

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let saved = background
                .spawn(async move { persist::save_competition(&connection, &dto).await })
                .await;

            this.update(cx, |this, cx| {
                match saved {
                    Ok(()) => {
                        this.summaries.push(CompetitionSummary {
                            id,
                            date,
                            name: name.clone(),
                        });
                        sort_summaries(&mut this.summaries);
                        if let Some(db) = this.db.as_ref() {
                            let connection = db.connection();
                            let doc =
                                cx.new(|cx| CompetitionDocument::new(competition, connection, cx));
                            this.track_document(id, &doc, cx);
                            this.open.insert(id, doc);
                            this.selected = Some(id);
                            this.recompute_undo_state(cx);
                        }
                        info!("created competition {id} (\"{name}\")");
                    }
                    Err(err) => {
                        error!("creating competition {id} failed: {err}");
                        this.status = Status::Failed(err.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Observe an open document so its sidebar summary and the aggregate dirty
    /// flag track its edits.
    fn track_document(
        &mut self,
        id: Uuid,
        doc: &Entity<CompetitionDocument>,
        cx: &mut Context<Self>,
    ) {
        let sub = cx.observe(doc, move |this, doc, cx| {
            this.sync_summary(id, &doc, cx);
            this.recompute_dirty(cx);
            this.recompute_undo_state(cx);
        });
        self.doc_subs.insert(id, sub);
        self.recompute_dirty(cx);
        self.recompute_undo_state(cx);
    }

    /// Re-derive `(can_undo, can_redo)` for the selected document; notify on
    /// change so the Edit menu / shell refresh.
    fn recompute_undo_state(&mut self, cx: &mut Context<Self>) {
        let state = self
            .selected_document()
            .map(|doc| {
                let doc = doc.read(cx);
                (doc.can_undo(), doc.can_redo())
            })
            .unwrap_or((false, false));
        if state != self.undo_state {
            self.undo_state = state;
            cx.notify();
        }
    }

    /// Recompute [`Self::any_dirty`] from the open documents; notify on change.
    fn recompute_dirty(&mut self, cx: &mut Context<Self>) {
        let any = self
            .open
            .values()
            .any(|doc| doc.read(cx).save_state() != SaveState::Saved);
        if any != self.any_dirty {
            self.any_dirty = any;
            cx.notify();
        }
    }

    /// Re-derive the sidebar summary (name / date) for one open document.
    fn sync_summary(
        &mut self,
        id: Uuid,
        doc: &Entity<CompetitionDocument>,
        cx: &mut Context<Self>,
    ) {
        let (name, date) = {
            let meta = doc.read(cx).metadata().read(cx).meta();
            (meta.name.clone(), meta.date)
        };
        if let Some(summary) = self.summaries.iter_mut().find(|s| s.id == id)
            && (summary.name != name || summary.date != date)
        {
            summary.name = name;
            summary.date = date;
            sort_summaries(&mut self.summaries);
            cx.notify();
        }
    }

    /// Duplicate a competition under a fresh id (a "{name} (Kopie)" copy of
    /// its persisted state) and select the copy. Flushes the source document
    /// first, if it is open, so the copy is current.
    pub fn duplicate_competition(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(db) = self.db.as_ref() else {
            warn!("duplicate_competition({id}) ignored — database not ready");
            return;
        };
        let load_connection = db.connection();
        let save_connection = db.connection();
        let flush = self
            .open
            .get(&id)
            .cloned()
            .map(|doc| doc.update(cx, |doc, cx| doc.flush_now(cx)));
        info!("duplicating competition {id}");

        cx.spawn(async move |this, cx| {
            if let Some(flush) = flush {
                flush.await;
            }
            let background = cx.background_executor().clone();
            let loaded = background
                .spawn(async move { persist::load_competition(&load_connection, id).await })
                .await;

            let mut dto = match loaded {
                Ok(Some(dto)) => dto,
                Ok(None) => {
                    warn!("duplicate_competition({id}): row gone");
                    return;
                }
                Err(err) => {
                    error!("duplicate_competition({id}): load failed: {err}");
                    return Self::report_error(&this, cx, err.to_string());
                }
            };

            let new_id = Uuid::new_v4();
            dto.id = new_id;
            dto.name =
                cx.update(|cx| cx.t_fmt("sidebar.duplicate-competition-label", &[("name", &dto.name)]));
            let date = dto.date;
            let name = dto.name.clone();

            let background = cx.background_executor().clone();
            let saved = background
                .spawn(async move { persist::save_competition(&save_connection, &dto).await })
                .await;

            match saved {
                Ok(()) => {
                    this.update(cx, |this, cx| {
                        this.summaries.push(CompetitionSummary {
                            id: new_id,
                            date,
                            name: name.clone(),
                        });
                        sort_summaries(&mut this.summaries);
                        this.select(new_id, cx);
                        info!("duplicated competition {id} → {new_id} (\"{name}\")");
                        cx.notify();
                    })
                    .ok();
                }
                Err(err) => {
                    error!("duplicate_competition({id}): saving the copy failed: {err}");
                    Self::report_error(&this, cx, err.to_string());
                }
            }
        })
        .detach();
    }

    /// Move a competition to the trash and close its open document. Updates
    /// the sidebar (and the trash list) optimistically, then writes the
    /// actual `deleted_at` in the background. See [`Self::purge_competition`]
    /// / [`Self::purge_all_trashed`] for the actually-irreversible step.
    pub fn delete_competition(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(db) = self.db.as_ref() else {
            warn!("delete_competition({id}) ignored — database not ready");
            return;
        };
        let connection = db.connection();
        let deleted_at = Utc::now();
        info!(
            "moving competition {id} (\"{}\") to the trash",
            self.name_of(id).unwrap_or("?")
        );

        self.open.remove(&id);
        self.doc_subs.remove(&id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        if let Some(pos) = self.summaries.iter().position(|s| s.id == id) {
            let summary = self.summaries.remove(pos);
            self.trashed.push(TrashedCompetition {
                id: summary.id,
                date: summary.date,
                name: summary.name,
                deleted_at,
            });
            sort_trashed(&mut self.trashed);
        }
        self.recompute_dirty(cx);
        self.recompute_undo_state(cx);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let deleted = background
                .spawn(async move {
                    persist::soft_delete_competition(&connection, id, deleted_at).await
                })
                .await;

            match deleted {
                Ok(()) => trace!("competition {id} moved to the trash"),
                Err(err) => {
                    error!("moving competition {id} to the trash failed: {err}");
                    this.update(cx, |this, cx| {
                        this.status = Status::Failed(err.to_string());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Move several competitions to the trash at once (the sidebar's
    /// multi-select bulk delete). Updates the sidebar/trash lists
    /// optimistically for every id, then issues a single
    /// `UPDATE … WHERE id IN (…)` in the background — spawning one
    /// [`Self::delete_competition`] per id instead raced N concurrent
    /// statements against the same `turso::Connection`, which turso rejects
    /// outright ("concurrent use forbidden") once enough are in flight at
    /// once.
    pub fn delete_competitions(&mut self, ids: Vec<Uuid>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let Some(db) = self.db.as_ref() else {
            warn!(
                "delete_competitions({} ids) ignored — database not ready",
                ids.len()
            );
            return;
        };
        let connection = db.connection();
        let deleted_at = Utc::now();
        info!("moving {} competition(s) to the trash", ids.len());

        let id_set: std::collections::HashSet<Uuid> = ids.iter().copied().collect();
        for &id in &ids {
            self.open.remove(&id);
            self.doc_subs.remove(&id);
            if self.selected == Some(id) {
                self.selected = None;
            }
        }
        self.summaries.retain(|s| {
            if !id_set.contains(&s.id) {
                return true;
            }
            self.trashed.push(TrashedCompetition {
                id: s.id,
                date: s.date,
                name: s.name.clone(),
                deleted_at,
            });
            false
        });
        sort_trashed(&mut self.trashed);
        self.recompute_dirty(cx);
        self.recompute_undo_state(cx);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let count = ids.len();
            let deleted = background
                .spawn(async move {
                    persist::soft_delete_competitions(&connection, &ids, deleted_at).await
                })
                .await;

            match deleted {
                Ok(()) => trace!("{count} competition(s) moved to the trash"),
                Err(err) => {
                    error!("moving {count} competition(s) to the trash failed: {err}");
                    this.update(cx, |this, cx| {
                        this.status = Status::Failed(err.to_string());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Restore a trashed competition back to the normal sidebar listing.
    /// Optimistic, same shape as every other mutating method here.
    pub fn restore_competition(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(db) = self.db.as_ref() else {
            warn!("restore_competition({id}) ignored — database not ready");
            return;
        };
        let connection = db.connection();
        let Some(pos) = self.trashed.iter().position(|t| t.id == id) else {
            return;
        };
        let trashed = self.trashed.remove(pos);
        info!("restoring competition {id} (\"{}\")", trashed.name);
        self.summaries.push(CompetitionSummary {
            id: trashed.id,
            date: trashed.date,
            name: trashed.name,
        });
        sort_summaries(&mut self.summaries);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let restored = background
                .spawn(async move { persist::restore_competition(&connection, id).await })
                .await;

            if let Err(err) = restored {
                error!("restoring competition {id} failed: {err}");
                this.update(cx, |this, cx| {
                    this.status = Status::Failed(err.to_string());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Permanently remove one trashed competition — the trash window's
    /// per-row "endgültig löschen". Unlike [`Self::delete_competition`],
    /// there is no undoing this.
    pub fn purge_competition(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(db) = self.db.as_ref() else {
            warn!("purge_competition({id}) ignored — database not ready");
            return;
        };
        let connection = db.connection();
        self.trashed.retain(|t| t.id != id);
        info!("purging trashed competition {id}");
        cx.notify();

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let purged = background
                .spawn(async move { persist::purge_competition(&connection, id).await })
                .await;

            if let Err(err) = purged {
                error!("purging competition {id} failed: {err}");
                this.update(cx, |this, cx| {
                    this.status = Status::Failed(err.to_string());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Empty the trash — permanently remove every trashed competition.
    pub fn purge_all_trashed(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.db.as_ref() else {
            warn!("purge_all_trashed ignored — database not ready");
            return;
        };
        let connection = db.connection();
        let count = self.trashed.len();
        if count == 0 {
            return;
        }
        self.trashed.clear();
        info!("emptying the trash ({count} competition(s))");
        cx.notify();

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let purged = background
                .spawn(async move { persist::purge_all_trashed(&connection).await })
                .await;

            match purged {
                Ok(removed) => trace!("emptied the trash — {removed} row(s) removed"),
                Err(err) => {
                    error!("emptying the trash failed: {err}");
                    this.update(cx, |this, cx| {
                        this.status = Status::Failed(err.to_string());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Write the selected competition's postcard blob to `dest` (a `.dtbke`
    /// file). Flushes the open document first so the export is current.
    ///
    /// The task resolves to whether the file was written completely, so a caller that still has to
    /// deliver it somewhere (iPadOS) can wait for that.
    pub fn export_competition(
        &mut self,
        id: Uuid,
        dest: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let Some(db) = self.db.as_ref() else {
            warn!("export_competition({id}) ignored — database not ready");
            return Task::ready(false);
        };
        let connection = db.connection();
        let flush = self
            .open
            .get(&id)
            .cloned()
            .map(|doc| doc.update(cx, |doc, cx| doc.flush_now(cx)));
        info!("exporting competition {id} → {}", dest.display());

        cx.spawn(async move |this, cx| {
            if let Some(flush) = flush {
                flush.await;
            }
            let background = cx.background_executor().clone();
            let loaded = background
                .spawn(async move { persist::load_competition(&connection, id).await })
                .await;

            let bytes = match loaded {
                Ok(Some(dto)) => persist::competition_dto_to_bytes(&dto),
                Ok(None) => {
                    warn!("export_competition({id}): row gone");
                    return false;
                }
                Err(err) => {
                    error!("export_competition({id}): load failed: {err}");
                    Self::report_error(&this, cx, err.to_string());
                    return false;
                }
            };
            match bytes.map(|bytes| std::fs::write(&dest, bytes)) {
                Ok(Ok(())) => {
                    info!("exported competition {id} → {}", dest.display());
                    true
                }
                Ok(Err(err)) => {
                    error!(
                        "export_competition({id}): writing {} failed: {err}",
                        dest.display()
                    );
                    let message = cx.update(|cx| {
                        cx.t_fmt("store.export-write-failed", &[("detail", &err.to_string())])
                    });
                    Self::report_error(&this, cx, message);
                    false
                }
                Err(err) => {
                    error!("export_competition({id}): serialising failed: {err}");
                    Self::report_error(&this, cx, err.to_string());
                    false
                }
            }
        })
    }

    /// Copy the whole database file to `dest`. Flushes every open document first.
    ///
    /// Like [`Self::export_competition`], the task resolves to whether the copy succeeded.
    pub fn export_all(&mut self, dest: PathBuf, cx: &mut Context<Self>) -> Task<bool> {
        for doc in self.open.values().cloned().collect::<Vec<_>>() {
            doc.update(cx, |doc, cx| doc.flush_now(cx).detach());
        }
        let source = FilesystemHelper::instance().database_path();
        info!("exporting the whole database → {}", dest.display());
        cx.spawn(async move |this, cx| {
            // Give the debounced writes a moment to land before copying.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(600))
                .await;
            match std::fs::copy(&source, &dest) {
                Ok(bytes) => {
                    info!("database exported ({bytes} bytes) → {}", dest.display());
                    true
                }
                Err(err) => {
                    error!(
                        "export_all: copying {} → {} failed: {err}",
                        source.display(),
                        dest.display()
                    );
                    let message = cx.update(|cx| {
                        cx.t_fmt("store.copy-failed", &[("detail", &err.to_string())])
                    });
                    Self::report_error(&this, cx, message);
                    false
                }
            }
        })
    }

    /// Write today's automatic backup, if one hasn't been written already and
    /// the setting is on. Called periodically from `AppShell`'s startup +
    /// recheck loop (see `updater`'s own loop for the identical shape this
    /// mirrors) — cheap to call repeatedly, since both of those checks make
    /// it a no-op almost every time. Unlike [`Self::export_all`] (a
    /// user-initiated action, reported through [`Self::report_error`] on
    /// failure), this is unattended housekeeping: every IO error is only
    /// logged, never surfaced.
    pub fn maybe_backup(&mut self, cx: &mut Context<Self>) {
        if !Settings::global(cx).auto_backup {
            return;
        }
        let today = Local::now().date_naive();
        let dest = FilesystemHelper::instance().backup_path_for(today);
        if dest.exists() {
            debug!("today's automatic backup already exists → {}", dest.display());
            return;
        }
        for doc in self.open.values().cloned().collect::<Vec<_>>() {
            doc.update(cx, |doc, cx| doc.flush_now(cx).detach());
        }
        let source = FilesystemHelper::instance().database_path();
        info!("creating today's automatic backup → {}", dest.display());
        cx.spawn(async move |_, cx| {
            // Give the debounced writes a moment to land before copying —
            // same wait `export_all` uses for the same reason.
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            match std::fs::copy(&source, &dest) {
                Ok(bytes) => {
                    info!("automatic backup written ({bytes} bytes) → {}", dest.display());
                    match FilesystemHelper::instance().gc_old_backups() {
                        0 => {}
                        removed => info!("backup gc: removed {removed} backup(s) older than 14 days"),
                    }
                }
                Err(err) => error!("automatic backup to {} failed: {err}", dest.display()),
            }
        })
        .detach();
    }

    /// Persist an imported competition (overwriting any existing row with the
    /// same id), refresh the sidebar, and select it.
    pub fn import_decoded(&mut self, dto: CompetitionDTO, cx: &mut Context<Self>) {
        let Some(db) = self.db.as_ref() else {
            warn!("import_decoded ignored — database not ready");
            return;
        };
        let connection = db.connection();
        let id = dto.id;
        let date = dto.date;
        let name = dto.name.clone();
        let overwrite = self.summaries.iter().any(|s| s.id == id);
        info!(
            "importing competition {id} (\"{name}\", {date}){}",
            if overwrite {
                " — overwriting an existing row"
            } else {
                ""
            }
        );

        // Drop any open editor for this id so a fresh one is built on select.
        self.open.remove(&id);
        self.doc_subs.remove(&id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        self.recompute_undo_state(cx);

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let saved = background
                .spawn(async move { persist::save_competition(&connection, &dto).await })
                .await;

            this.update(cx, |this, cx| {
                match saved {
                    Ok(()) => {
                        if let Some(existing) = this.summaries.iter_mut().find(|s| s.id == id) {
                            existing.date = date;
                            existing.name = name.clone();
                        } else {
                            this.summaries.push(CompetitionSummary {
                                id,
                                date,
                                name: name.clone(),
                            });
                        }
                        sort_summaries(&mut this.summaries);
                        this.select(id, cx);
                        info!("imported competition {id} (\"{name}\")");
                    }
                    Err(err) => {
                        error!("import of competition {id} failed to persist: {err}");
                        this.status = Status::Failed(err.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Perf stress test only (`stress.rs`): persist many competitions in one
    /// background pass, list them all, then select `select`.
    pub fn seed_many(&mut self, dtos: Vec<CompetitionDTO>, select: Uuid, cx: &mut Context<Self>) {
        let Some(db) = self.db.as_ref() else {
            warn!("seed_many ignored — database not ready");
            return;
        };
        let connection = db.connection();
        let summaries: Vec<CompetitionSummary> = dtos
            .iter()
            .map(|d| CompetitionSummary {
                id: d.id,
                date: d.date,
                name: d.name.clone(),
            })
            .collect();
        info!("stress: seeding {} competitions", dtos.len());

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let saved = background
                .spawn(async move {
                    for dto in &dtos {
                        persist::save_competition(&connection, dto).await?;
                    }
                    Ok::<(), persist::PersistError>(())
                })
                .await;
            this.update(cx, |this, cx| {
                match saved {
                    Ok(()) => {
                        this.summaries.extend(summaries);
                        sort_summaries(&mut this.summaries);
                        this.select(select, cx);
                    }
                    Err(err) => error!("stress: seeding failed: {err}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn report_error(this: &WeakEntity<Self>, cx: &mut AsyncApp, message: String) {
        warn!("store error surfaced to the UI: {message}");
        this.update(cx, |this, cx| {
            this.status = Status::Failed(message);
            cx.notify();
        })
        .ok();
    }

    async fn bootstrap(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        let path = FilesystemHelper::instance().database_path();
        debug!("opening the database at {}", path.display());

        // One-off: open on the foreground executor so the non-Send
        // `turso::Database` stays on the main thread.
        let db = match Db::open(&path).await {
            Ok(db) => db,
            Err(err) => {
                error!("opening the database at {} failed: {err}", path.display());
                this.update(cx, |this, cx| {
                    this.status = Status::Failed(err.to_string());
                    cx.notify();
                })
                .ok();
                return;
            }
        };

        let connection = db.connection();
        let trash_connection = db.connection();
        let background = cx.background_executor().clone();
        let listed = background
            .spawn(async move { persist::list_summaries(&connection).await })
            .await;
        let listed_trash = background
            .spawn(async move { persist::list_trashed(&trash_connection).await })
            .await;

        this.update(cx, |this, cx| {
            this.db = Some(db);
            match listed {
                Ok(mut rows) => {
                    sort_summaries(&mut rows);
                    info!("database ready — {} competition(s)", rows.len());
                    this.summaries = rows;
                    this.status = Status::Ready;
                }
                Err(err) => {
                    error!("listing competitions failed: {err}");
                    this.status = Status::Failed(err.to_string());
                }
            }
            match listed_trash {
                Ok(mut rows) => {
                    sort_trashed(&mut rows);
                    debug!("trash ready — {} competition(s)", rows.len());
                    this.trashed = rows;
                }
                Err(err) => error!("listing trashed competitions failed: {err}"),
            }
            cx.notify();
        })
        .ok();
    }
}

fn sort_summaries(summaries: &mut [CompetitionSummary]) {
    // Newest first — both the year sections and the rows within them.
    summaries.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn sort_trashed(trashed: &mut [TrashedCompetition]) {
    // Most recently deleted first — matches `list_trashed`'s own ordering.
    trashed.sort_by_key(|t| std::cmp::Reverse(t.deleted_at));
}

/// Sensible starting values for the "new competition" dialog (name blank — the
/// user must fill it in).
pub fn default_new_meta() -> CompetitionMeta {
    CompetitionMeta {
        name: String::new(),
        organization: OrganizationDTO::DTB,
        location: String::new(),
        date: Local::now().date_naive(),
        meeting_times: MeetingTimeDTO::Unified(
            NaiveTime::from_hms_opt(9, 0, 0).expect("09:00 is a valid time"),
        ),
        responsible_persons: Vec::new(),
    }
}
