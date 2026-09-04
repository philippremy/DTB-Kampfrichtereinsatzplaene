//! A somewhat-unified "save / export" dialog across platforms.
//!
//! gpui only offers a bare file-path picker, so this module talks to the OS
//! directly to get a *save panel with format + option controls*:
//!
//! * **macOS** — `NSSavePanel` with an accessory view ([`macos`]).
//! * **Windows** — `IFileSaveDialog` + `IFileDialogCustomize` ([`windows_dlg`]).
//! * **Linux / other** — an in-app gpui window collects the format + options,
//!   then gpui's plain path picker takes the filename ([`fallback`]).
//!
//! All three resolve to the same [`SaveChoice`]. Set `DTB_KE_SAVE_FALLBACK=1`
//! to force the in-app path on any OS.

use std::path::PathBuf;

use dtb_ke_export::{DocxExport, PdfExport, PdfStandard};
use futures::channel::oneshot;
use gpui::App;

mod fallback;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows_dlg;

/// What the user chose in the save dialog.
#[derive(Clone, Debug)]
pub struct SaveChoice {
    pub path: PathBuf,
    pub format: ExportFormat,
}

/// One export target, with its options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Pdf(PdfExport),
    /// A Word-idiomatic `.docx` (named styles; `embed_fonts` optionally embeds
    /// the Archivo weights).
    Docx(DocxExport),
    /// The raw postcard blob (`.dtbke`) — a backup / transfer copy.
    Blob,
}

impl ExportFormat {
    /// The starting selection.
    pub fn default_choice() -> Self {
        ExportFormat::Pdf(PdfExport::default())
    }

    pub fn kind(&self) -> FormatKind {
        match self {
            ExportFormat::Pdf(_) => FormatKind::Pdf,
            ExportFormat::Docx(_) => FormatKind::Docx,
            ExportFormat::Blob => FormatKind::Blob,
        }
    }

    pub fn extension(&self) -> &'static str {
        self.kind().extension()
    }

    /// The PDF standards selected (empty unless this is [`ExportFormat::Pdf`]).
    fn pdf_standards(&self) -> &[PdfStandard] {
        match self {
            ExportFormat::Pdf(o) => &o.standards,
            _ => &[],
        }
    }

    fn docx_embed_fonts(&self) -> bool {
        matches!(self, ExportFormat::Docx(o) if o.embed_fonts)
    }
}

/// The three format families, for the dialog's format selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatKind {
    Pdf,
    Docx,
    Blob,
}

impl FormatKind {
    /// In dialog order.
    pub const ALL: [Self; 3] = [Self::Pdf, Self::Docx, Self::Blob];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pdf => "Portable Document Format (.pdf)",
            Self::Docx => "Word-Dokument (.docx)",
            Self::Blob => "Rohdaten (.dtbke)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Blob => "dtbke",
        }
    }

    /// A UTI (macOS) / filter description usable for the OS dialog.
    pub fn uti(self) -> &'static str {
        match self {
            Self::Pdf => "com.adobe.pdf",
            Self::Docx => "org.openxmlformats.wordprocessingml.document",
            Self::Blob => "de.philippremy.DTB-Kampfrichtreinsatzpläne.raw-competition",
        }
    }

    /// Rebuild an [`ExportFormat`] of this family, carrying `previous`'s options
    /// where they still apply.
    pub fn with_options(self, previous: &ExportFormat) -> ExportFormat {
        match self {
            Self::Pdf => ExportFormat::Pdf(PdfExport {
                standards: previous.pdf_standards().to_vec(),
            }),
            Self::Docx => ExportFormat::Docx(DocxExport {
                embed_fonts: previous.docx_embed_fonts(),
            }),
            Self::Blob => ExportFormat::Blob,
        }
    }
}

/// Show the platform save dialog. Resolves to `None` if the user cancels.
pub fn prompt(default_name: String, cx: &mut App) -> oneshot::Receiver<Option<SaveChoice>> {
    if std::env::var_os("DTB_KE_SAVE_FALLBACK").is_some() {
        log::debug!("save dialog: DTB_KE_SAVE_FALLBACK set — using the in-app panel");
        return fallback::prompt(default_name, cx);
    }
    log::debug!("save dialog: opening the native save panel for \"{default_name}\"");

    #[cfg(target_os = "macos")]
    {
        macos::prompt(default_name, cx)
    }
    #[cfg(target_os = "windows")]
    {
        windows_dlg::prompt(default_name, cx)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        fallback::prompt(default_name, cx)
    }
}

/// Ensure `path` ends in the format's extension (the native panels usually do
/// this themselves; the fallback and a stubborn user might not).
pub(crate) fn with_extension(mut path: PathBuf, format: &ExportFormat) -> PathBuf {
    let ext = format.extension();
    if path.extension().and_then(|e| e.to_str()) != Some(ext) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if let Some(name) = name {
            path.set_file_name(format!("{name}.{ext}"));
        }
    }
    path
}
