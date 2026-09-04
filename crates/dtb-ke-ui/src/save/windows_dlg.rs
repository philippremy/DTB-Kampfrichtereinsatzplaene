//! Windows `IFileSaveDialog` + `IFileDialogCustomize`: the file-type dropdown
//! is the format selector; check buttons carry the PDF / DOCX options.
//!
//! A `DialogEvents` COM object ([`windows::core::implement`]) is advised on the
//! dialog so it behaves like the macOS panel:
//!
//! * `OnTypeChange` shows only the option group for the chosen format.
//! * `OnCheckButtonToggled` re-runs [`pdf_standard_conflicts`] and disables the
//!   PDF standards that would clash, appending the reason to their label (there
//!   is no per-control tooltip API for customised dialog controls).
//!
//! Untested from the dev machine — the FFI is type-checked against `windows`
//! 0.61 in an isolated crate; the workspace itself can't cross-compile to
//! Windows (turso's build script).

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
            .map(|(i, s)| (CHECK_PDF_BASE + i as u32, *s, HSTRING::from(s.label())))
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
    /// `(control id, standard, original label)` per PDF-standard checkbox.
    pdf_checks: Vec<(u32, PdfStandard, HSTRING)>,
}

impl DialogEvents {
    /// Disable the PDF standards that clash with the ticked ones and append the
    /// reason to their label; restore the rest.
    fn refresh_conflicts(&self, customize: &IFileDialogCustomize) -> windows::core::Result<()> {
        let selected: Vec<PdfStandard> = self
            .pdf_checks
            .iter()
            .filter(|(id, _, _)| {
                unsafe { customize.GetCheckButtonState(*id) }
                    .map(|b| b.as_bool())
                    .unwrap_or(false)
            })
            .map(|(_, standard, _)| *standard)
            .collect();
        let conflicts = pdf_standard_conflicts(&selected);

        for (id, standard, original) in &self.pdf_checks {
            match conflicts.iter().find(|(s, _)| s == standard) {
                Some((_, reason)) => unsafe {
                    customize.SetControlState(*id, CDCS_VISIBLE)?; // visible, disabled
                    customize
                        .SetControlLabel(*id, &HSTRING::from(format!("{original} – {reason}")))?;
                },
                None => unsafe {
                    customize.SetControlState(*id, CDCS_ENABLEDVISIBLE)?;
                    customize.SetControlLabel(*id, original)?;
                },
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
        _dwidctl: u32,
        _bchecked: BOOL,
    ) -> windows::core::Result<()> {
        self.refresh_conflicts(pfdc.ok()?)
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
