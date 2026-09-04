use std::time::Duration;

use chrono::NaiveDate;
use dtb_ke_persist as persist;
use dtb_ke_types::{CompetitionDTO, JudgingTableKindDTO, MeetingTimeDTO, OrganizationDTO};
use gpui::{App, AppContext, Context, Entity, EventEmitter, Subscription, Task};
use log::{debug, error, trace};
use turso::Connection;
use uuid::Uuid;

use crate::model::{Competition, CompetitionMeta, JudgingTable, RichText, Round};

/// Fallback autosave debounce if the settings global is not installed (tests).
const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// How long editing must pause before the accumulated change becomes one undo
/// step. Short enough to feel responsive, long enough that a burst of typing
/// folds into a single entry.
const CHECKPOINT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Upper bound on the undo history (each entry is one `CompetitionDTO`).
const HISTORY_LIMIT: usize = 100;

/// Emitted by [`CompetitionDocument`] when [`CompetitionDocument::undo`] /
/// [`CompetitionDocument::redo`] has swapped the whole document state — the
/// detail view rebinds its child editors in response.
#[derive(Clone, Copy, Debug)]
pub enum DocumentEvent {
    Restored,
}

/// Snapshot-based undo/redo for one document.
///
/// `baseline` is the state as of the last checkpoint; a checkpoint is taken
/// after editing settles ([`CHECKPOINT_DEBOUNCE`]). `undo` / `redo` hold whole
/// [`CompetitionDTO`] snapshots. `undo` / `redo` set `baseline` to the target,
/// so the edit notifications the following restore triggers see no delta and
/// don't record a spurious checkpoint.
#[derive(Default)]
struct History {
    baseline: Option<CompetitionDTO>,
    undo: Vec<CompetitionDTO>,
    redo: Vec<CompetitionDTO>,
}

impl History {
    /// Record `now` as a new checkpoint if it differs from the baseline. When it
    /// does, the old baseline becomes an undo step and the redo stack is
    /// dropped. Returns whether anything was recorded.
    fn checkpoint(&mut self, now: CompetitionDTO) -> bool {
        if self.baseline.as_ref() == Some(&now) {
            return false;
        }
        if let Some(previous) = self.baseline.replace(now) {
            self.undo.push(previous);
            if self.undo.len() > HISTORY_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        true
    }

    /// Pop an undo step. `current` (the live state) moves onto the redo stack;
    /// the popped snapshot becomes the new baseline and is returned for the
    /// caller to apply.
    fn pop_undo(&mut self, current: CompetitionDTO) -> Option<CompetitionDTO> {
        let target = self.undo.pop()?;
        self.redo.push(current);
        self.baseline = Some(target.clone());
        Some(target)
    }

    /// The mirror of [`Self::pop_undo`].
    fn pop_redo(&mut self, current: CompetitionDTO) -> Option<CompetitionDTO> {
        let target = self.redo.pop()?;
        self.undo.push(current);
        self.baseline = Some(target.clone());
        Some(target)
    }
}

/// One open competition.
///
/// Holds the fine-grained sub-editors (each its own [`Entity`], so a keystroke
/// in one judging table re-renders only that table) and owns the debounced
/// write-back of the whole competition blob. It observes every sub-editor;
/// any change marks the document dirty and (re)arms the autosave timer.
pub struct CompetitionDocument {
    id: Uuid,
    connection: Connection,

    metadata: Entity<MetadataEditor>,
    qualification: Entity<RoundEditor>,
    finale: Entity<RoundEditor>,
    remarks: Entity<RemarksEditor>,

    dirty: bool,
    saving: bool,
    last_error: Option<String>,

    history: History,
    /// The pending undo-checkpoint debounce. Dropping it cancels the timer.
    checkpoint: Option<Task<()>>,

    /// Observers on the four sub-editors. Rebuilt is unnecessary — the editors
    /// are mutated in place, never swapped — so this is set once.
    _subscriptions: Vec<Subscription>,
    /// The pending debounce timer. Replacing it (on every edit) drops the old
    /// `Task`, which cancels the previous timer — that is the debounce.
    autosave: Option<Task<()>>,
}

/// Derived, display-oriented view of the document's persistence state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveState {
    Saved,
    Dirty,
    Saving,
    Error(String),
}

impl CompetitionDocument {
    pub fn new(competition: Competition, connection: Connection, cx: &mut Context<Self>) -> Self {
        let Competition {
            id,
            meta,
            qualification,
            finale,
            remarks,
        } = competition;

        let metadata = cx.new(|_| MetadataEditor { meta });
        let qualification = cx.new(|cx| RoundEditor::new(qualification, cx));
        let finale = cx.new(|cx| RoundEditor::new(finale, cx));
        let remarks = cx.new(|_| RemarksEditor { text: remarks });

        let mut subscriptions = vec![
            cx.observe(&metadata, |this, _, cx| this.on_changed(cx)),
            cx.observe(&qualification, |this, _, cx| this.on_changed(cx)),
            cx.observe(&finale, |this, _, cx| this.on_changed(cx)),
            cx.observe(&remarks, |this, _, cx| this.on_changed(cx)),
        ];
        // Best-effort flush of unsaved edits when the app is quitting.
        subscriptions.push(cx.on_app_quit(|this, cx| this.flush_now(cx)));

        let mut this = Self {
            id,
            connection,
            metadata,
            qualification,
            finale,
            remarks,
            dirty: false,
            saving: false,
            last_error: None,
            history: History::default(),
            checkpoint: None,
            _subscriptions: subscriptions,
            autosave: None,
        };
        this.history.baseline = Some(this.snapshot(cx));
        this
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn metadata(&self) -> &Entity<MetadataEditor> {
        &self.metadata
    }

    pub fn qualification(&self) -> &Entity<RoundEditor> {
        &self.qualification
    }

    pub fn finale(&self) -> &Entity<RoundEditor> {
        &self.finale
    }

    pub fn remarks(&self) -> &Entity<RemarksEditor> {
        &self.remarks
    }

    pub fn save_state(&self) -> SaveState {
        if let Some(err) = &self.last_error {
            SaveState::Error(err.clone())
        } else if self.saving {
            SaveState::Saving
        } else if self.dirty {
            SaveState::Dirty
        } else {
            SaveState::Saved
        }
    }

    /// Force an immediate write if there are unsaved changes; the returned task
    /// resolves once the write completes. Used on app quit and when switching
    /// away from this competition.
    pub fn flush_now(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.flush(cx)
    }

    fn on_changed(&mut self, cx: &mut Context<Self>) {
        let was_clean = !self.dirty;
        self.dirty = true;
        self.last_error = None;
        cx.notify();

        // Fold this edit (and any that follow within the window) into one undo
        // step once editing settles.
        self.checkpoint = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHECKPOINT_DEBOUNCE).await;
            this.update(cx, |this, cx| this.commit_checkpoint(cx)).ok();
        }));

        let delay = cx
            .try_global::<crate::settings::Settings>()
            .map(|s| s.autosave.duration())
            .unwrap_or(AUTOSAVE_DEBOUNCE);
        if was_clean {
            debug!(
                "{} became dirty — autosave armed ({} ms)",
                self.id,
                delay.as_millis()
            );
        } else {
            trace!("{} edited — autosave re-armed", self.id);
        }
        self.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                let task = this.flush(cx);
                task.detach();
            })
            .ok();
        }));
    }

    /// Serialize the current state and start a background write, unless nothing
    /// changed or a write is already in flight. Returns a task that resolves
    /// when the write finishes.
    fn flush(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.saving || !self.dirty {
            return Task::ready(());
        }

        let dto = self.snapshot(cx);
        let connection = self.connection.clone();
        let id = self.id;

        self.dirty = false;
        self.saving = true;
        self.last_error = None;
        cx.notify();
        debug!("{id}: writing competition blob");

        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let result = background
                .spawn(async move { persist::save_competition(&connection, &dto).await })
                .await;

            this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(()) => debug!("{id}: saved"),
                    Err(err) => {
                        error!("{id}: autosave failed — {err}");
                        this.last_error = Some(err.to_string());
                        this.dirty = true;
                    }
                }
                cx.notify();

                // Edits landed while we were writing — save again.
                if this.dirty {
                    trace!("{id}: edits arrived mid-write — flushing again");
                    let task = this.flush(cx);
                    task.detach();
                }
            })
            .ok();
        })
    }

    /// The current state as a [`CompetitionDTO`] — for the Typst preview /
    /// export, which need the wire form without waiting for an autosave.
    pub fn to_dto(&self, cx: &App) -> CompetitionDTO {
        self.snapshot(cx)
    }

    /// Reassemble a [`CompetitionDTO`] from the sub-editors.
    fn snapshot(&self, cx: &App) -> CompetitionDTO {
        let competition = Competition {
            id: self.id,
            meta: self.metadata.read(cx).meta.clone(),
            qualification: self.qualification.read(cx).to_round(cx),
            finale: self.finale.read(cx).to_round(cx),
            remarks: self.remarks.read(cx).text.clone(),
        };
        CompetitionDTO::from(&competition)
    }

    // ── undo / redo ──────────────────────────────────────────────────────

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// Fold every edit since the last checkpoint into one undo step. Cheap and
    /// idempotent when nothing changed — called on the debounce timer, and
    /// synchronously before an undo / redo so no edit is stranded.
    fn commit_checkpoint(&mut self, cx: &mut Context<Self>) {
        self.checkpoint = None;
        let now = self.snapshot(cx);
        if self.history.checkpoint(now) {
            cx.notify();
        }
    }

    /// Revert to the previous checkpoint. No-op when there is nothing to undo.
    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.commit_checkpoint(cx);
        let current = self.snapshot(cx);
        let Some(target) = self.history.pop_undo(current) else {
            return;
        };
        debug!("{}: undo ({} left)", self.id, self.history.undo.len());
        self.restore(target, cx);
    }

    /// Re-apply the most recently undone checkpoint. No-op when there is nothing
    /// to redo.
    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.commit_checkpoint(cx);
        let current = self.snapshot(cx);
        let Some(target) = self.history.pop_redo(current) else {
            return;
        };
        debug!("{}: redo ({} left)", self.id, self.history.redo.len());
        self.restore(target, cx);
    }

    /// Push `target` into every sub-editor. The history baseline is already
    /// `target` (set by `pop_undo` / `pop_redo`), so the change notifications
    /// this triggers (dirty flag, autosave, a fresh checkpoint timer) see no
    /// delta and leave the history untouched. Emits [`DocumentEvent::Restored`]
    /// so the detail view rebuilds.
    fn restore(&mut self, target: CompetitionDTO, cx: &mut Context<Self>) {
        let Competition {
            meta,
            qualification,
            finale,
            remarks,
            ..
        } = Competition::from(target);

        self.metadata
            .update(cx, |editor, cx| editor.replace(meta, cx));
        self.remarks
            .update(cx, |editor, cx| editor.set_text(remarks, cx));
        self.qualification
            .update(cx, |editor, cx| editor.replace(qualification, cx));
        self.finale
            .update(cx, |editor, cx| editor.replace(finale, cx));

        cx.emit(DocumentEvent::Restored);
        cx.notify();
    }
}

impl EventEmitter<DocumentEvent> for CompetitionDocument {}

/// Editor for the non-table metadata of a competition.
pub struct MetadataEditor {
    meta: CompetitionMeta,
}

impl MetadataEditor {
    pub fn meta(&self) -> &CompetitionMeta {
        &self.meta
    }

    /// Overwrite the whole metadata record (undo / redo). Notifies
    /// unconditionally — the caller only restores when something changed.
    pub fn replace(&mut self, meta: CompetitionMeta, cx: &mut Context<Self>) {
        self.meta = meta;
        cx.notify();
    }

    pub fn set_name(&mut self, name: String, cx: &mut Context<Self>) {
        if self.meta.name != name {
            debug!("metadata: name → {name:?}");
            self.meta.name = name;
            cx.notify();
        }
    }

    pub fn set_location(&mut self, location: String, cx: &mut Context<Self>) {
        if self.meta.location != location {
            self.meta.location = location;
            cx.notify();
        }
    }

    pub fn set_date(&mut self, date: NaiveDate, cx: &mut Context<Self>) {
        if self.meta.date != date {
            debug!("metadata: date → {date}");
            self.meta.date = date;
            cx.notify();
        }
    }

    pub fn set_organization(&mut self, organization: OrganizationDTO, cx: &mut Context<Self>) {
        if self.meta.organization != organization {
            debug!("metadata: organization → {organization:?}");
            self.meta.organization = organization;
            cx.notify();
        }
    }

    pub fn set_meeting_times(&mut self, meeting_times: MeetingTimeDTO, cx: &mut Context<Self>) {
        if self.meta.meeting_times != meeting_times {
            trace!("metadata: meeting times changed");
            self.meta.meeting_times = meeting_times;
            cx.notify();
        }
    }

    /// Mutable access to the responsible-persons list. Calling this assumes a
    /// mutation follows and notifies observers unconditionally.
    pub fn responsible_persons_mut(&mut self, cx: &mut Context<Self>) -> &mut Vec<String> {
        cx.notify();
        &mut self.meta.responsible_persons
    }
}

/// Editor for one competition phase: an ordered list of judging-table editors
/// plus the phase's reserve judges.
pub struct RoundEditor {
    tables: Vec<Entity<JudgingTableEditor>>,
    spare_judges: Vec<String>,
    /// Kept index-aligned with `tables`; dropping an entry unsubscribes.
    table_subscriptions: Vec<Subscription>,
}

impl RoundEditor {
    fn new(round: Round, cx: &mut Context<Self>) -> Self {
        let mut tables = Vec::with_capacity(round.tables.len());
        let mut table_subscriptions = Vec::with_capacity(round.tables.len());
        for table in round.tables {
            let entity = cx.new(|_| JudgingTableEditor { table });
            table_subscriptions.push(cx.observe(&entity, |_, _, cx| cx.notify()));
            tables.push(entity);
        }
        Self {
            tables,
            spare_judges: round.spare_judges,
            table_subscriptions,
        }
    }

    pub fn tables(&self) -> &[Entity<JudgingTableEditor>] {
        &self.tables
    }

    pub fn spare_judges(&self) -> &[String] {
        &self.spare_judges
    }

    /// Replace the whole phase (undo / redo). Every table editor is rebuilt, so
    /// the detail view must rebuild its cards — it does, on
    /// [`DocumentEvent::Restored`](super::DocumentEvent).
    pub fn replace(&mut self, round: Round, cx: &mut Context<Self>) {
        self.tables.clear();
        self.table_subscriptions.clear();
        for table in round.tables {
            let entity = cx.new(|_| JudgingTableEditor { table });
            self.table_subscriptions
                .push(cx.observe(&entity, |_, _, cx| cx.notify()));
            self.tables.push(entity);
        }
        self.spare_judges = round.spare_judges;
        cx.notify();
    }

    pub fn add_table(&mut self, table: JudgingTable, cx: &mut Context<Self>) {
        debug!(
            "round: adding judging table \"{}\" ({:?})",
            table.label, table.kind
        );
        let entity = cx.new(|_| JudgingTableEditor { table });
        self.table_subscriptions
            .push(cx.observe(&entity, |_, _, cx| cx.notify()));
        self.tables.push(entity);
        cx.notify();
    }

    pub fn remove_table(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tables.len() {
            debug!("round: removing judging table #{index}");
            self.tables.remove(index);
            // Dropping the subscription unsubscribes from that table editor.
            drop(self.table_subscriptions.remove(index));
            cx.notify();
        } else {
            trace!(
                "round: remove_table({index}) out of range ({} tables)",
                self.tables.len()
            );
        }
    }

    /// Move the table at `from` to position `to` (drag-reorder). Keeps
    /// `table_subscriptions` index-aligned.
    pub fn move_table(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let last = self.tables.len();
        if from == to || from >= last || to >= last {
            return;
        }
        trace!("round: moving judging table {from} → {to}");
        let table = self.tables.remove(from);
        let sub = self.table_subscriptions.remove(from);
        self.tables.insert(to, table);
        self.table_subscriptions.insert(to, sub);
        cx.notify();
    }

    pub fn set_spare_judges(&mut self, spare_judges: Vec<String>, cx: &mut Context<Self>) {
        if self.spare_judges != spare_judges {
            self.spare_judges = spare_judges;
            cx.notify();
        }
    }

    fn to_round(&self, cx: &App) -> Round {
        Round {
            tables: self
                .tables
                .iter()
                .map(|entity| entity.read(cx).table.clone())
                .collect(),
            spare_judges: self.spare_judges.clone(),
        }
    }
}

/// Editor for a single judging table.
pub struct JudgingTableEditor {
    table: JudgingTable,
}

impl JudgingTableEditor {
    pub fn table(&self) -> &JudgingTable {
        &self.table
    }

    pub fn label(&self) -> &str {
        &self.table.label
    }

    pub fn set_label(&mut self, label: String, cx: &mut Context<Self>) {
        if self.table.label != label {
            self.table.label = label;
            cx.notify();
        }
    }

    pub fn set_kind(&mut self, kind: JudgingTableKindDTO, cx: &mut Context<Self>) {
        if self.table.kind != kind {
            debug!(
                "judging table \"{}\": discipline → {:?}",
                self.table.label, kind
            );
            self.table.kind = kind;
            cx.notify();
        }
    }

    /// Mutable access to the discipline-specific assignments (edit one judge
    /// slot, then let go). Notifies observers unconditionally.
    pub fn kind_mut(&mut self, cx: &mut Context<Self>) -> &mut JudgingTableKindDTO {
        cx.notify();
        &mut self.table.kind
    }
}

/// Editor for the free-form styled text at the end of the document.
pub struct RemarksEditor {
    text: RichText,
}

impl RemarksEditor {
    pub fn text(&self) -> &RichText {
        &self.text
    }

    pub fn set_text(&mut self, text: RichText, cx: &mut Context<Self>) {
        if self.text != text {
            self.text = text;
            cx.notify();
        }
    }

    /// Mutable access to the rich-text tree. Notifies observers unconditionally.
    pub fn text_mut(&mut self, cx: &mut Context<Self>) -> &mut RichText {
        cx.notify();
        &mut self.text
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;
    use dtb_ke_types::{
        JudgingTablesDTO, MeetingTimeDTO, OrganizationDTO, RichTextDTO, SpareJudgesDTO,
    };

    use super::*;

    /// A snapshot whose only distinguishing feature is its `name` — enough to
    /// track it through the history.
    fn snap(name: &str) -> CompetitionDTO {
        CompetitionDTO {
            id: Uuid::nil(),
            name: name.to_owned(),
            organization: OrganizationDTO::DTB,
            location: String::new(),
            date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            meeting_times: MeetingTimeDTO::Unified(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            responsible_persons: Vec::new(),
            spare_judges: SpareJudgesDTO::default(),
            judging_tables: JudgingTablesDTO::default(),
            additional_remarks: RichTextDTO::default(),
        }
    }

    fn history_from(baseline: &str) -> History {
        History {
            baseline: Some(snap(baseline)),
            ..History::default()
        }
    }

    #[test]
    fn checkpoint_records_only_on_a_real_change() {
        let mut h = history_from("a");
        assert!(!h.checkpoint(snap("a")), "identical state is not a step");
        assert!(h.undo.is_empty());

        assert!(h.checkpoint(snap("b")));
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.baseline.as_ref().unwrap().name, "b");
    }

    #[test]
    fn undo_then_redo_round_trips() {
        let mut h = history_from("a");
        h.checkpoint(snap("b"));
        h.checkpoint(snap("c")); // undo: [a, b], baseline c

        assert_eq!(h.pop_undo(snap("c")).unwrap().name, "b");
        assert_eq!(h.pop_undo(snap("b")).unwrap().name, "a");
        assert!(h.pop_undo(snap("a")).is_none());

        assert_eq!(h.pop_redo(snap("a")).unwrap().name, "b");
        assert_eq!(h.pop_redo(snap("b")).unwrap().name, "c");
        assert!(h.pop_redo(snap("c")).is_none());
    }

    #[test]
    fn a_fresh_edit_after_undo_drops_the_redo_branch() {
        let mut h = history_from("a");
        h.checkpoint(snap("b"));
        h.pop_undo(snap("b")); // baseline a, redo: [b]
        assert_eq!(h.redo.len(), 1);

        assert!(h.checkpoint(snap("c")));
        assert!(h.redo.is_empty(), "the redo branch is discarded");
        assert_eq!(h.undo.last().unwrap().name, "a");
    }

    #[test]
    fn the_undo_stack_is_capped() {
        let mut h = history_from("start");
        for i in 0..(HISTORY_LIMIT + 25) {
            h.checkpoint(snap(&format!("edit-{i}")));
        }
        assert_eq!(h.undo.len(), HISTORY_LIMIT);
        // The oldest entries were dropped; "start" is long gone.
        assert!(h.undo.iter().all(|s| s.name != "start"));
    }
}
