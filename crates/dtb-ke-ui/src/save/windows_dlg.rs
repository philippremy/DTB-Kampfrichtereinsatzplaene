//! Windows `IFileSaveDialog` + `IFileDialogCustomize`: the file-type dropdown
//! is the format selector; check buttons carry the PDF / DOCX options.
//!
//! A `DialogEvents` COM object ([`windows::core::implement`]) is advised on the
//! dialog so it behaves like the macOS panel:
//!
//! * `OnTypeChange` shows only the option group for the chosen format —
//!   `SetControlState` on a *group* id (from `StartVisualGroup`/
//!   `EndVisualGroup`) does cascade to every control added inside it, per
//!   Microsoft's own docs ("doing so affects all of the controls within
//!   it") — confirmed against the actual API reference, not assumed.
//! * `OnCheckButtonToggled` re-runs [`pdf_standard_conflicts`] and disables
//!   the PDF standards that would clash — **only** when the toggled control
//!   was actually one of the PDF-standard checkboxes. A real bug (caught
//!   from a live run, not guessed): this used to run unconditionally for
//!   *any* checkbox, including DOCX's "Schriften einbetten" — which then
//!   force-set every PDF checkbox's *individual* state back to visible,
//!   overriding the PDF group's own `CDCS_INACTIVE` from `OnTypeChange`
//!   while DOCX was selected (individual control state is independent of,
//!   and can outlive, a `SetControlState` call on the group). That's what
//!   showed up as "the PDF options are somehow still present [under DOCX]
//!   and push the DOCX option out of view", and left the dialog in a
//!   genuinely inconsistent state that then rendered wrong even after
//!   switching back to PDF.
//! * A disabled standard's reason is **not** appended to its own checkbox
//!   label any more (it used to be — full German sentences turned each
//!   checkbox into a very long line, which is what actually ballooned the
//!   dialog's width). `IFileDialogCustomize` has no per-control tooltip API
//!   either (confirmed against the interface's own method list — there is
//!   `SetControlLabel`, nothing tooltip-shaped) and no hover event on
//!   `IFileDialogControlEvents`, so a real on-hover tooltip isn't available
//!   through this interface at all. The practical alternative: one shared,
//!   plain-text control (`CONFLICT_INFO`) below the PDF checkboxes,
//!   populated with every current conflict's reason (already a full,
//!   self-naming sentence — see `dtb_ke_export::pdf_standard_conflicts`) and
//!   hidden outright when nothing conflicts.
//!
//! Compiles clean for the real target now (`cargo check -p dtb-ke-ui --target
//! x86_64-pc-windows-gnullvm`, confirmed this session — the workspace-wide
//! cross-compile blocker noted elsewhere turned out to be stale), but this
//! file's actual on-screen behaviour still hasn't been run on real Windows.

use std::path::PathBuf;

use dtb_ke_export::{DocxExport, PdfExport, PdfStandard, pdf_standard_conflicts};
use futures::channel::oneshot;
use gpui::App;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    CDCS_ENABLEDVISIBLE, CDCS_INACTIVE, CDCS_VISIBLE, FDE_OVERWRITE_RESPONSE,
    FDE_SHAREVIOLATION_RESPONSE, FDEOR_DEFAULT, FDESVR_DEFAULT, FileSaveDialog, IFileDialog,
    IFileDialogControlEvents, IFileDialogControlEvents_Impl, IFileDialogCustomize,
    IFileDialogEvents, IFileDialogEvents_Impl, IFileSaveDialog, IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::{BOOL, HSTRING, Interface, Ref, implement, w};

use crate::save::{ExportFormat, FormatKind, SaveChoice, with_extension};

/// Control ids for the customised dialog controls.
const GROUP_PDF: u32 = 100;
const GROUP_DOCX: u32 = 101;
const CHECK_PDF_BASE: u32 = 110; // + PdfStandard index
/// Plain-text control listing why any currently-disabled PDF standard is
/// disabled (see the module doc comment — there is no tooltip API for this).
const CONFLICT_INFO: u32 = 190;
const CHECK_DOCX_EMBED: u32 = 200;

pub(super) fn prompt(default_name: String, cx: &mut App) -> oneshot::Receiver<Option<SaveChoice>> {
    let (tx, rx) = oneshot::channel();

    // gpui initialises COM on the main thread; the modal dialog pumps messages.
    cx.foreground_executor()
        .spawn(async move {
            let choice = show_dialog(&default_name).unwrap_or_else(|err| {
                log::error!("Export-Dialog fehlgeschlagen: {err}");
                None
            });
            let _ = tx.send(choice);
        })
        .detach();

    rx
}

fn show_dialog(default_name: &str) -> windows::core::Result<Option<SaveChoice>> {
    let dialog: IFileSaveDialog = unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_ALL)? };

    unsafe {
        dialog.SetTitle(w!("Wettkampf exportieren"))?;
        dialog.SetFileName(&HSTRING::from(default_name.trim()))?;
        dialog.SetFileTypes(&[
            COMDLG_FILTERSPEC {
                pszName: w!("PDF-Dokument"),
                pszSpec: w!("*.pdf"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("Word-Dokument"),
                pszSpec: w!("*.docx"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("DTB-KE-Datei (Sicherungskopie)"),
                pszSpec: w!("*.dtbke"),
            },
        ])?;
        dialog.SetFileTypeIndex(1)?; // 1-based
        dialog.SetDefaultExtension(w!("pdf"))?;
    }

    // ── customised option controls ──────────────────────────────────────────
    let customize: IFileDialogCustomize = dialog.cast()?;
    unsafe {
        customize.StartVisualGroup(GROUP_PDF, w!("PDF-Standards"))?;
        for (i, standard) in PdfStandard::ALL.iter().enumerate() {
            customize.AddCheckButton(
                CHECK_PDF_BASE + i as u32,
                &HSTRING::from(standard.label()),
                false,
            )?;
        }
        // Hidden until refresh_conflicts finds something to say — see the
        // module doc comment for why this exists instead of a tooltip.
        customize.AddText(CONFLICT_INFO, w!(""))?;
        customize.SetControlState(CONFLICT_INFO, CDCS_INACTIVE)?;
        customize.EndVisualGroup()?;

        customize.StartVisualGroup(GROUP_DOCX, w!("DOCX-Optionen"))?;
        customize.AddCheckButton(CHECK_DOCX_EMBED, w!("Schriften einbetten"), false)?;
        customize.EndVisualGroup()?;

        // Start on the PDF format → hide the DOCX group.
        customize.SetControlState(GROUP_DOCX, CDCS_INACTIVE)?;
    }

    // ── advise the events object ────────────────────────────────────────────
    let events: IFileDialogEvents = DialogEvents {
        pdf_checks: PdfStandard::ALL
            .iter()
            .enumerate()
            .map(|(i, s)| (CHECK_PDF_BASE + i as u32, *s))
            .collect(),
    }
    .into();
    let cookie = unsafe { dialog.Advise(&events)? };

    // ── show ────────────────────────────────────────────────────────────────
    let shown = unsafe { dialog.Show(None) };
    unsafe { dialog.Unadvise(cookie).ok() };
    if shown.is_err() {
        return Ok(None); // cancelled
    }

    let item: IShellItem = unsafe { dialog.GetResult()? };
    let path = unsafe {
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let text = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0 as _));
        PathBuf::from(text)
    };

    let type_index = unsafe { dialog.GetFileTypeIndex()? };
    let kind = match type_index {
        2 => FormatKind::Docx,
        3 => FormatKind::Blob,
        _ => FormatKind::Pdf,
    };

    let format = match kind {
        FormatKind::Pdf => {
            let standards = PdfStandard::ALL
                .iter()
                .enumerate()
                .filter(|(i, _)| {
                    unsafe { customize.GetCheckButtonState(CHECK_PDF_BASE + *i as u32) }
                        .map(|b| b.as_bool())
                        .unwrap_or(false)
                })
                .map(|(_, s)| *s)
                .collect();
            ExportFormat::Pdf(PdfExport { standards })
        }
        FormatKind::Docx => ExportFormat::Docx(DocxExport {
            embed_fonts: unsafe { customize.GetCheckButtonState(CHECK_DOCX_EMBED) }
                .map(|b| b.as_bool())
                .unwrap_or(false),
        }),
        FormatKind::Blob => ExportFormat::Blob,
    };

    Ok(Some(SaveChoice {
        path: with_extension(path, &format),
        format,
    }))
}

/// Advised on the dialog: reacts to format changes and PDF-standard toggles.
#[implement(IFileDialogEvents, IFileDialogControlEvents)]
struct DialogEvents {
    /// `(control id, standard)` per PDF-standard checkbox.
    pdf_checks: Vec<(u32, PdfStandard)>,
}

impl DialogEvents {
    /// Disable the PDF standards that clash with the ticked ones and list why
    /// in [`CONFLICT_INFO`] (see the module doc comment — no per-control
    /// tooltip API exists to hang the reason off the checkbox itself).
    fn refresh_conflicts(&self, customize: &IFileDialogCustomize) -> windows::core::Result<()> {
        let selected: Vec<PdfStandard> = self
            .pdf_checks
            .iter()
            .filter(|(id, _)| {
                unsafe { customize.GetCheckButtonState(*id) }
                    .map(|b| b.as_bool())
                    .unwrap_or(false)
            })
            .map(|(_, standard)| *standard)
            .collect();
        let conflicts = pdf_standard_conflicts(&selected);

        for (id, standard) in &self.pdf_checks {
            let state = if conflicts.iter().any(|(s, _)| s == standard) {
                CDCS_VISIBLE // visible, disabled
            } else {
                CDCS_ENABLEDVISIBLE
            };
            unsafe { customize.SetControlState(*id, state)? };
        }

        unsafe {
            if conflicts.is_empty() {
                customize.SetControlState(CONFLICT_INFO, CDCS_INACTIVE)?;
            } else {
                let message = conflicts
                    .iter()
                    .map(|(_, reason)| reason.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                customize.SetControlLabel(CONFLICT_INFO, &HSTRING::from(message))?;
                customize.SetControlState(CONFLICT_INFO, CDCS_ENABLEDVISIBLE)?;
            }
        }
        Ok(())
    }
}

#[allow(non_snake_case)]
impl IFileDialogEvents_Impl for DialogEvents_Impl {
    fn OnTypeChange(&self, pfd: Ref<'_, IFileDialog>) -> windows::core::Result<()> {
        let pfd = pfd.ok()?;
        let customize: IFileDialogCustomize = pfd.cast()?;
        let (pdf, docx) = match unsafe { pfd.GetFileTypeIndex()? } {
            1 => (CDCS_ENABLEDVISIBLE, CDCS_INACTIVE),
            2 => (CDCS_INACTIVE, CDCS_ENABLEDVISIBLE),
            _ => (CDCS_INACTIVE, CDCS_INACTIVE),
        };
        unsafe {
            customize.SetControlState(GROUP_PDF, pdf)?;
            customize.SetControlState(GROUP_DOCX, docx)?;
        }
        if pdf == CDCS_ENABLEDVISIBLE {
            self.refresh_conflicts(&customize)?;
        }
        Ok(())
    }

    fn OnFileOk(&self, _pfd: Ref<'_, IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnFolderChanging(
        &self,
        _pfd: Ref<'_, IFileDialog>,
        _psi: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnFolderChange(&self, _pfd: Ref<'_, IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnSelectionChange(&self, _pfd: Ref<'_, IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnShareViolation(
        &self,
        _pfd: Ref<'_, IFileDialog>,
        _psi: Ref<'_, IShellItem>,
    ) -> windows::core::Result<FDE_SHAREVIOLATION_RESPONSE> {
        Ok(FDESVR_DEFAULT)
    }
    fn OnOverwrite(
        &self,
        _pfd: Ref<'_, IFileDialog>,
        _psi: Ref<'_, IShellItem>,
    ) -> windows::core::Result<FDE_OVERWRITE_RESPONSE> {
        Ok(FDEOR_DEFAULT)
    }
}

#[allow(non_snake_case)]
impl IFileDialogControlEvents_Impl for DialogEvents_Impl {
    fn OnCheckButtonToggled(
        &self,
        pfdc: Ref<'_, IFileDialogCustomize>,
        dwidctl: u32,
        _bchecked: BOOL,
    ) -> windows::core::Result<()> {
        // Only a PDF-standard checkbox changes the conflict set. This used to
        // run unconditionally — see the module doc comment for the real bug
        // that caused (DOCX's own "Schriften einbetten" toggle forced every
        // PDF checkbox's state back to visible, fighting the PDF group's own
        // `CDCS_INACTIVE` from `OnTypeChange`).
        if self.pdf_checks.iter().any(|(id, _)| *id == dwidctl) {
            self.refresh_conflicts(pfdc.ok()?)?;
        }
        Ok(())
    }

    fn OnItemSelected(
        &self,
        _pfdc: Ref<'_, IFileDialogCustomize>,
        _dwidctl: u32,
        _dwiditem: u32,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnButtonClicked(
        &self,
        _pfdc: Ref<'_, IFileDialogCustomize>,
        _dwidctl: u32,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnControlActivating(
        &self,
        _pfdc: Ref<'_, IFileDialogCustomize>,
        _dwidctl: u32,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}
