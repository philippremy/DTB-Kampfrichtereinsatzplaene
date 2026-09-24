//! macOS `NSSavePanel` with a live accessory view.
//!
//! Presented as a **sheet** on the key window (Apple's convention — no window
//! chrome, blocks its parent, can't be opened twice); falls back to a modeless
//! panel only if there is no window to attach to.
//!
//! A format pop-up drives which option rows are shown (PDF standards / DOCX
//! toggle / a note for the raw blob); only the rows for the selected format are
//! visible. The accessory is a plain `NSView` the panel stretches edge-to-edge,
//! with the option stack **pinned centred by Auto Layout** (`centerXAnchor`), so
//! hiding a section only changes the height — horizontal centring is a hard
//! constraint and can never flash left-aligned (which manual `setFrameSize`
//! re-fitting used to do). Within the PDF rows, ticking a standard disables the
//! ones Typst would reject in combination and hangs an explanatory tooltip off
//! them.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use block2::RcBlock;
use dtb_ke_export::{DocxExport, PdfExport, PdfStandard, pdf_standard_conflicts};
use futures::channel::oneshot;
use gpui_kit::App;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSButton, NSColor, NSControlStateValueOn, NSLayoutAttribute, NSModalResponse,
    NSModalResponseOK, NSPopUpButton, NSSavePanel, NSStackView, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSEdgeInsets, NSString, NSURL};

use crate::i18n::{ActiveLocale, Locale};
use crate::save::{
    ExportFormat, FormatKind, SaveChoice, pdf_standard_conflict_message, pdf_standard_label,
    with_extension,
};

/// The lowest width we let the accessory view shrink to.
const MIN_ACCESSORY_WIDTH: f64 = 360.0;

pub(super) fn prompt(default_name: String, cx: &mut App) -> oneshot::Receiver<Option<SaveChoice>> {
    let (tx, rx) = oneshot::channel();
    let locale = cx.global::<Locale>().clone();

    cx.foreground_executor()
        .spawn(async move {
            let Some(mtm) = MainThreadMarker::new() else {
                let _ = tx.send(None);
                return;
            };

            let base_name = default_name.trim().to_owned();

            let panel = NSSavePanel::savePanel(mtm);
            panel.setPrompt(Some(&ns(&locale.t("save.window-title"))));
            panel.setCanCreateDirectories(true);

            let popup = NSPopUpButton::new(mtm);
            for kind in FormatKind::ALL {
                popup.addItemWithTitle(&ns(&kind.label(&locale)));
            }
            popup.selectItemAtIndex(0);

            // ── PDF section ──────────────────────────────────────────────────
            let pdf_section = section(mtm, &locale.t("save.standards-label"));
            let pdf_rows = column(mtm, NSLayoutAttribute::Leading, 4.0);
            let pdf_checks: Vec<Retained<NSButton>> = PdfStandard::ALL
                .iter()
                .map(|standard| {
                    // SAFETY: target / action are wired below once the controller exists.
                    let cb = unsafe {
                        NSButton::checkboxWithTitle_target_action(
                            &ns(&pdf_standard_label(*standard, &locale)),
                            None,
                            None,
                            mtm,
                        )
                    };
                    pdf_rows.addArrangedSubview(&cb);
                    cb
                })
                .collect();
            pdf_section.addArrangedSubview(&pdf_rows);

            // ── DOCX section ────────────────────────────────────────────────
            let docx_section = section(mtm, &locale.t("save.docx-options-label"));
            // SAFETY: no target / action — the checkbox only carries state.
            let docx_check = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &ns(&locale.t("save.embed-fonts-label")),
                    None,
                    None,
                    mtm,
                )
            };
            docx_check.setState(NSControlStateValueOn);
            docx_section.addArrangedSubview(&docx_check);

            // ── blob section ────────────────────────────────────────────────
            let blob_section = section(mtm, &locale.t("save.blob-section-label"));
            blob_section.addArrangedSubview(&label(mtm, &locale.t("save.blob-description")));

            // ── accessory: a full-width container with a centred option stack ──
            let options = column(mtm, NSLayoutAttribute::CenterX, 8.0);
            options.setEdgeInsets(NSEdgeInsets {
                top: 16.0,
                left: 24.0,
                bottom: 16.0,
                right: 24.0,
            });
            options.addArrangedSubview(&label(mtm, &locale.t("save.format-label")));
            options.addArrangedSubview(&popup);
            options.addArrangedSubview(&pdf_section);
            options.addArrangedSubview(&docx_section);
            options.addArrangedSubview(&blob_section);
            options.setTranslatesAutoresizingMaskIntoConstraints(false);

            let accessory = NSView::new(mtm);
            accessory.setTranslatesAutoresizingMaskIntoConstraints(false);
            accessory.addSubview(&options);
            // centre horizontally + drive the accessory's height; keep it on
            // screen and no narrower than MIN_ACCESSORY_WIDTH.
            options
                .centerXAnchor()
                .constraintEqualToAnchor(&accessory.centerXAnchor())
                .setActive(true);
            options
                .topAnchor()
                .constraintEqualToAnchor(&accessory.topAnchor())
                .setActive(true);
            options
                .bottomAnchor()
                .constraintEqualToAnchor(&accessory.bottomAnchor())
                .setActive(true);
            options
                .leadingAnchor()
                .constraintGreaterThanOrEqualToAnchor_constant(&accessory.leadingAnchor(), 24.)
                .setActive(true);
            options
                .trailingAnchor()
                .constraintGreaterThanOrEqualToAnchor_constant(&accessory.trailingAnchor(), 24.)
                .setActive(true);
            options
                .widthAnchor()
                .constraintGreaterThanOrEqualToConstant(MIN_ACCESSORY_WIDTH)
                .setActive(true);

            panel.setAccessoryView(Some(&accessory));

            let ui = Rc::new(Ui {
                base_name,
                locale: locale.clone(),
                panel: panel.clone(),
                accessory: accessory.clone(),
                popup: popup.clone(),
                pdf_section,
                docx_section,
                blob_section,
                pdf_checks,
                docx_check: docx_check.clone(),
            });

            let controller = Controller::new(ui.clone());
            let target: &AnyObject = &controller;
            unsafe {
                popup.setTarget(Some(target));
                popup.setAction(Some(sel!(formatChanged:)));
                for cb in &ui.pdf_checks {
                    cb.setTarget(Some(target));
                    cb.setAction(Some(sel!(standardToggled:)));
                }
            }

            ui.sync_format();

            // ── run it ──────────────────────────────────────────────────────
            let slot: Cell<Option<oneshot::Sender<Option<SaveChoice>>>> = Cell::new(Some(tx));
            let ui_for_block = ui.clone();
            let keep_controller = controller; // AppKit's target ref is weak — hold it here.
            let handler = RcBlock::new(move |response: NSModalResponse| {
                let _ = &keep_controller;
                let choice = read_choice(response, &ui_for_block);
                if let Some(tx) = slot.take() {
                    let _ = tx.send(choice);
                }
            });

            // Present as a sheet on the active window (no chrome, blocks its
            // parent). Only fall back to a free-floating panel with no window.
            let app = NSApplication::sharedApplication(mtm);
            let parent = app.keyWindow().or_else(|| app.mainWindow());
            match parent {
                Some(window) => panel.beginSheetModalForWindow_completionHandler(&window, &handler),
                None => panel.beginWithCompletionHandler(&handler),
            }
        })
        .detach();

    rx
}

/// Everything the action handlers need to touch. Held by the [`Controller`] and
/// by the completion block.
struct Ui {
    base_name: String,
    locale: Locale,
    panel: Retained<NSSavePanel>,
    /// The full-width container view handed to the panel.
    accessory: Retained<NSView>,
    popup: Retained<NSPopUpButton>,
    pdf_section: Retained<NSStackView>,
    docx_section: Retained<NSStackView>,
    blob_section: Retained<NSStackView>,
    /// Parallel to [`PdfStandard::ALL`].
    pdf_checks: Vec<Retained<NSButton>>,
    docx_check: Retained<NSButton>,
}

impl Ui {
    fn selected_kind(&self) -> FormatKind {
        let index = self.popup.indexOfSelectedItem().max(0) as usize;
        FormatKind::ALL
            .get(index)
            .copied()
            .unwrap_or(FormatKind::Pdf)
    }

    /// Show only the rows for the current format and refresh the filename
    /// extension. Auto Layout re-flows the (centred) option stack and the panel
    /// picks up the new accessory height on its own.
    fn sync_format(&self) {
        let kind = self.selected_kind();
        self.pdf_section.setHidden(kind != FormatKind::Pdf);
        self.docx_section.setHidden(kind != FormatKind::Docx);
        self.blob_section.setHidden(kind != FormatKind::Blob);

        self.panel.setNameFieldStringValue(&ns(&format!(
            "{}.{}",
            self.base_name,
            kind.extension()
        )));

        if kind == FormatKind::Pdf {
            self.sync_standards();
        }

        // Nudge the panel to re-measure the accessory's new height. Horizontal
        // position is a constraint, so it stays centred regardless.
        self.accessory.layoutSubtreeIfNeeded();
        self.panel.setAccessoryView(Some(&self.accessory));
    }

    /// Disable the PDF standards that clash with what is already ticked and give
    /// each a tooltip saying why.
    fn sync_standards(&self) {
        let selected: Vec<PdfStandard> = PdfStandard::ALL
            .iter()
            .zip(&self.pdf_checks)
            .filter(|(_, cb)| cb.state() == NSControlStateValueOn)
            .map(|(standard, _)| *standard)
            .collect();
        let conflicts = pdf_standard_conflicts(&selected);

        for (standard, cb) in PdfStandard::ALL.iter().zip(&self.pdf_checks) {
            match conflicts.iter().find(|(s, _)| s == standard) {
                Some((_, conflict)) => {
                    let reason = pdf_standard_conflict_message(*standard, *conflict, &self.locale);
                    cb.setEnabled(false);
                    cb.setToolTip(Some(&ns(&reason)));
                }
                None => {
                    cb.setEnabled(true);
                    cb.setToolTip(None);
                }
            }
        }
    }
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Controller` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "DTBKeSaveAccessoryController"]
    #[ivars = Rc<Ui>]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    impl Controller {
        #[unsafe(method(formatChanged:))]
        fn format_changed(&self, _sender: *mut AnyObject) {
            self.ivars().sync_format();
        }

        #[unsafe(method(standardToggled:))]
        fn standard_toggled(&self, _sender: *mut AnyObject) {
            self.ivars().sync_standards();
        }
    }
);

impl Controller {
    fn new(ui: Rc<Ui>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ui);
        unsafe { msg_send![super(this), init] }
    }
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

/// A vertical `NSStackView` with the given cross-axis alignment.
fn column(
    mtm: MainThreadMarker,
    alignment: NSLayoutAttribute,
    spacing: f64,
) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(alignment);
    stack.setSpacing(spacing);
    stack
}

/// A centred section: a caption label above whatever the caller adds.
fn section(mtm: MainThreadMarker, title: &str) -> Retained<NSStackView> {
    let stack = column(mtm, NSLayoutAttribute::CenterX, 6.0);
    stack.addArrangedSubview(&label(mtm, title));
    stack
}

fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&ns(text), mtm);
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field
}

fn read_choice(response: NSModalResponse, ui: &Ui) -> Option<SaveChoice> {
    if response != NSModalResponseOK {
        return None;
    }

    let url: Retained<NSURL> = ui.panel.URL()?;
    let path = PathBuf::from(url.path()?.to_string());

    let format = match ui.selected_kind() {
        FormatKind::Pdf => {
            let standards = PdfStandard::ALL
                .iter()
                .zip(&ui.pdf_checks)
                .filter(|(_, cb)| cb.state() == NSControlStateValueOn)
                .map(|(standard, _)| *standard)
                .collect();
            ExportFormat::Pdf(PdfExport { standards })
        }
        FormatKind::Docx => ExportFormat::Docx(DocxExport {
            embed_fonts: ui.docx_check.state() == NSControlStateValueOn,
        }),
        FormatKind::Blob => ExportFormat::Blob,
    };

    Some(SaveChoice {
        target: crate::save::Target::Path(with_extension(path, &format)),
        format,
    })
}
