//! Typst-backed export and preview for a *Kampfrichtereinsatzplan*.
//!
//! One [`Exporter`] owns a single in-memory Typst [`World`](world) and is
//! reused across compiles: the template, fonts and standard library stay
//! cached, only the competition data (`/data.json`) is swapped, so an
//! incremental re-compile — as the preview window will do on every edit — is
//! cheap.
//!
//! ```no_run
//! # use dtb_ke_export::{Exporter, PreviewOptions};
//! # fn demo(comp: &dtb_ke_types::CompetitionDTO) -> Result<(), dtb_ke_export::ExportError> {
//! let exporter = Exporter::new()?;
//! let pdf = exporter.compile_pdf(comp, &dtb_ke_export::PdfExport::default())?;
//! let pages = exporter.render_previews(comp, PreviewOptions::default())?;
//! # let _ = (pdf, pages);
//! # Ok(())
//! # }
//! ```

mod disciplines;
mod docx;
mod model;
mod svg;
mod world;

use std::sync::Mutex;
use std::time::Instant;

use chrono::{Datelike, Timelike};
use dtb_ke_types::{LandesturnverbandDTO, OrganizationDTO};
use log::{debug, trace, warn};
use typst::diag::SourceDiagnostic;
use typst::ecow::EcoVec;
use typst::foundations::{Datetime, Smart};
use typst_layout::PagedDocument;
use typst_pdf::Timestamp;

use crate::world::TypstWorld;

/// How many `comemo` generations to keep after each compile.
const CACHE_GENERATIONS: usize = 8;

/// Builds documents and previews for one competition at a time.
///
/// Cheap to keep around for the life of the app; not cheap to construct
/// (decodes the fonts), so make it once.
pub struct Exporter {
    world: Mutex<TypstWorld>,
}

impl Exporter {
    /// Construct the exporter, loading fonts and the template.
    pub fn new() -> Result<Self, ExportError> {
        let started = Instant::now();
        let world = TypstWorld::new()?;
        debug!(
            "exporter ready — fonts + template loaded in {:?}",
            started.elapsed()
        );
        Ok(Self {
            world: Mutex::new(world),
        })
    }

    /// Compile the competition to a PDF, enforcing the requested standards.
    pub fn compile_pdf(
        &self,
        competition: &CompetitionDTO,
        options: &PdfExport,
    ) -> Result<Vec<u8>, ExportError> {
        let started = Instant::now();
        debug!(
            "PDF export of \"{}\" — standards {:?}",
            competition.name, options.standards
        );
        let document = self.compile(competition)?;
        let pdf_options = typst_pdf::PdfOptions {
            standards: map_pdf_standards(&options.standards)?,
            creator: Smart::Custom(Some("DTB Kampfrichtereinsatzpläne".into())),
            tagged: true,
            pretty: true,
            timestamp: {
                let now = chrono::Utc::now();
                Datetime::from_ymd_hms(
                    now.year(),
                    now.month() as u8,
                    now.day() as u8,
                    now.hour() as u8,
                    now.minute() as u8,
                    now.second() as u8,
                )
                .map(Timestamp::new_utc)
            },
            ..Default::default()
        };
        let out = typst_pdf::pdf(&document, &pdf_options).map_err(|d| {
            let msgs = messages(&d);
            warn!("PDF write failed: {}", msgs.join("; "));
            ExportError::Compile(msgs)
        })?;
        debug!(
            "PDF export done — {} pages, {} bytes, {:?}",
            document.pages().len(),
            out.len(),
            started.elapsed()
        );
        Ok(out)
    }

    /// Compile the competition to a Word-idiomatic `.docx`.
    pub fn compile_docx(
        &self,
        competition: &CompetitionDTO,
        options: &DocxExport,
    ) -> Result<Vec<u8>, ExportError> {
        // No Typst here — the DOCX builder consumes the same digested model
        // (`crate::model`) as the template does, so there is still one
        // DTO→document transform.
        let model = model::build(competition, None, None, None);

        // Header artwork: this competition's org emblem (→ DTB emblem → DTB
        // word-mark), plus the "TURNEN!" swoosh — same resolution order as the
        // Typst path in `compile`.
        let org_logo = dtb_ke_resource::emblem(&org_slug(&competition.organization))
            .or_else(|| dtb_ke_resource::emblem("dtb"))
            .or_else(dtb_ke_resource::logo_dtb);
        let turnen_logo = dtb_ke_resource::logo_turnen();
        let logos = docx::HeaderLogos {
            org: org_logo.as_deref(),
            turnen: turnen_logo.as_deref(),
        };

        let started = Instant::now();
        debug!(
            "DOCX export of \"{}\" — embed_fonts = {}",
            competition.name, options.embed_fonts
        );
        let out = docx::build(&model, options, &logos);
        match &out {
            Ok(bytes) => debug!(
                "DOCX export done — {} bytes, {:?}",
                bytes.len(),
                started.elapsed()
            ),
            Err(err) => warn!("DOCX export failed: {err}"),
        }
        out
    }

    /// Rasterise every page for an on-screen preview.
    pub fn render_previews(
        &self,
        competition: &CompetitionDTO,
        options: PreviewOptions,
    ) -> Result<Vec<PageImage>, ExportError> {
        let document = self.compile(competition)?;
        let render_options = typst_render::RenderOptions {
            pixel_per_pt: (options.pixels_per_point.max(0.1) as f64).into(),
            render_bleed: false,
        };
        Ok(document
            .pages()
            .iter()
            .map(|page| {
                let pixmap = typst_render::render(page, &render_options);
                PageImage {
                    width: pixmap.width(),
                    height: pixmap.height(),
                    rgba: pixmap.take(),
                }
            })
            .collect())
    }

    fn compile(&self, competition: &CompetitionDTO) -> Result<PagedDocument, ExportError> {
        let world = self.world.lock().unwrap_or_else(|p| {
            warn!("exporter world mutex was poisoned by an earlier panic — recovering");
            p.into_inner()
        });

        // Header emblem, top-left: this competition's organisation, the DTB
        // emblem, then the DTB word-mark — whichever is committed first.
        let emblem = dtb_ke_resource::emblem(&org_slug(&competition.organization))
            .or_else(|| dtb_ke_resource::emblem("dtb"))
            .or_else(dtb_ke_resource::logo_dtb)
            .map(|c| c.into_owned());
        // The exact on-page frame (cm) so the template draws the emblem flush
        // against the header's left edge rather than centred in a fixed box.
        let emblem_frame_cm = emblem.as_deref().and_then(svg::emblem_frame_cm);
        world.set_emblem(emblem);

        let model = model::build(
            competition,
            world.emblem_path(),
            emblem_frame_cm,
            world.asset_path("turnen"),
        );
        let json = serde_json::to_string(&model)?;
        trace!("typst compile — data.json is {} bytes", json.len());
        world.set_data(&json);

        let started = Instant::now();
        let result = typst::compile::<PagedDocument>(&*world).output;
        comemo::evict(CACHE_GENERATIONS);
        match result {
            Ok(doc) => {
                trace!(
                    "typst compile ok — {} pages, {:?}",
                    doc.pages().len(),
                    started.elapsed()
                );
                Ok(doc)
            }
            Err(d) => {
                let msgs = messages(&d);
                warn!(
                    "typst compile failed ({:?}): {}",
                    started.elapsed(),
                    msgs.join("; ")
                );
                Err(ExportError::Compile(msgs))
            }
        }
    }
}

pub use dtb_ke_types::CompetitionDTO;

/// One rasterised page: row-major RGBA8. Pages are opaque (white ground), so
/// premultiplied and straight alpha coincide.
#[derive(Clone)]
pub struct PageImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Preview rasterisation settings.
#[derive(Clone, Copy, Debug)]
pub struct PreviewOptions {
    /// Output pixels per typographic point. `2.0` ≈ 144 dpi; raise for HiDPI.
    pub pixels_per_point: f32,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            pixels_per_point: 2.0,
        }
    }
}

/// A PDF standard the user can opt into. A curated subset of the standards
/// Typst supports; several may be combined (they are validated together).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfStandard {
    /// PDF 2.0 (the default output targets PDF 1.7).
    V2_0,
    /// PDF/A-2b — long-term archiving.
    A2b,
    /// PDF/A-3b — archiving, permits embedded source files.
    A3b,
    /// PDF/A-4 — archiving, based on PDF 2.0.
    A4,
    /// PDF/UA-1 — accessibility (tagged, defined reading order).
    Ua1,
}

impl PdfStandard {
    /// Every choice, in menu order.
    pub const ALL: [Self; 5] = [Self::A2b, Self::A3b, Self::A4, Self::Ua1, Self::V2_0];

    /// A short German label for the dialog.
    pub fn label(self) -> &'static str {
        match self {
            Self::V2_0 => "PDF 2.0",
            Self::A2b => "PDF/A-2b (Archiv)",
            Self::A3b => "PDF/A-3b (Archiv, mit Anhängen)",
            Self::A4 => "PDF/A-4 (Archiv, PDF 2.0)",
            Self::Ua1 => "PDF/UA-1 (Barrierefreiheit)",
        }
    }
}

/// Options for [`Exporter::compile_pdf`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PdfExport {
    /// Standards to enforce. Empty = a plain tagged PDF 1.7.
    pub standards: Vec<PdfStandard>,
}

/// Options for [`Exporter::compile_docx`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocxExport {
    /// Embed the fonts used in the document.
    pub embed_fonts: bool,
}

/// Everything that can go wrong producing a document.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no fonts are available to the exporter")]
    NoFonts,
    #[error("document template error: {0}")]
    Template(String),
    #[error("Typst compilation failed:\n{}", .0.join("\n"))]
    Compile(Vec<String>),
    #[error("could not serialise the document model: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("incompatible PDF standards: {0}")]
    PdfStandards(String),
    #[error("{0} ist noch nicht verfügbar")]
    NotImplemented(&'static str),
}

/// Check whether a combination of [`PdfStandard`]s is mutually compatible.
/// `Err` carries a human-readable German-ish reason (from Typst).
pub fn validate_pdf_standards(list: &[PdfStandard]) -> Result<(), String> {
    map_pdf_standards(list).map(|_| ()).map_err(|e| match e {
        ExportError::PdfStandards(msg) => msg,
        other => other.to_string(),
    })
}

/// Given the standards the user has already ticked, returns each *other*
/// curated [`PdfStandard`] that cannot be added on top, paired with a short
/// German explanation suitable for a tooltip.
///
/// The compatibility verdict is Typst's own matrix check
/// ([`validate_pdf_standards`]); only the wording of the reason is ours. Testing
/// each candidate against the *whole* current selection means n-ary conflicts
/// (a triple that is invalid though every pair is fine) are handled too.
pub fn pdf_standard_conflicts(selected: &[PdfStandard]) -> Vec<(PdfStandard, String)> {
    PdfStandard::ALL
        .iter()
        .copied()
        .filter(|candidate| !selected.contains(candidate))
        .filter(|candidate| {
            let mut trial = selected.to_vec();
            trial.push(*candidate);
            validate_pdf_standards(&trial).is_err()
        })
        .map(|candidate| (candidate, conflict_reason(selected, candidate)))
        .collect()
}

fn conflict_reason(selected: &[PdfStandard], candidate: PdfStandard) -> String {
    let is_archival =
        |s: PdfStandard| matches!(s, PdfStandard::A2b | PdfStandard::A3b | PdfStandard::A4);

    if is_archival(candidate)
        && let Some(existing) = selected.iter().copied().find(|s| is_archival(*s))
    {
        return format!(
            "Nur ein PDF/A-Standard ist gleichzeitig möglich – „{}“ ist bereits gewählt.",
            existing.label()
        );
    }

    // Everything else in the curated set is a PDF-version clash.
    match (
        required_version(candidate),
        selected.iter().copied().find_map(required_version),
    ) {
        (Some(needs), Some(has)) if needs != has => format!(
            "„{}“ benötigt {needs}; die aktuelle Auswahl legt {has} fest.",
            candidate.label()
        ),
        _ => format!(
            "„{}“ lässt sich nicht mit der aktuellen Auswahl kombinieren.",
            candidate.label()
        ),
    }
}

/// The PDF version a standard pins, where it is fixed and well-known.
fn required_version(s: PdfStandard) -> Option<&'static str> {
    match s {
        PdfStandard::V2_0 | PdfStandard::A4 => Some("PDF 2.0"),
        PdfStandard::A2b | PdfStandard::A3b => Some("PDF 1.7"),
        PdfStandard::Ua1 => None,
    }
}

fn map_pdf_standards(list: &[PdfStandard]) -> Result<typst_pdf::PdfStandards, ExportError> {
    use typst_pdf::PdfStandard as T;
    let mapped: Vec<T> = list
        .iter()
        .map(|s| match s {
            PdfStandard::V2_0 => T::V_2_0,
            PdfStandard::A2b => T::A_2b,
            PdfStandard::A3b => T::A_3b,
            PdfStandard::A4 => T::A_4,
            PdfStandard::Ua1 => T::Ua_1,
        })
        .collect();
    typst_pdf::PdfStandards::new(&mapped).map_err(|e| {
        let mut msg = e.message().to_string();
        for hint in e.hints() {
            msg.push_str(" — ");
            msg.push_str(hint);
        }
        ExportError::PdfStandards(msg)
    })
}

/// The stable slug for an organisation, used to look up its emblem in
/// `dtb-ke-resource` (`emblems/{slug}.svg`).
pub fn org_slug(org: &OrganizationDTO) -> String {
    match org {
        OrganizationDTO::DTB => "dtb".to_owned(),
        OrganizationDTO::IRV => "irv".to_owned(),
        OrganizationDTO::LFV(lfv) => format!("lfv-{}", lfv_slug(lfv)),
    }
}

fn lfv_slug(lfv: &LandesturnverbandDTO) -> &'static str {
    use LandesturnverbandDTO as L;
    match lfv {
        L::LfvBadischerTurnerBund => "badischer-turner-bund",
        L::LfvBayerischerTurnverband => "bayerischer-turnverband",
        L::LfvBerlinerTurnUndFreizeitsportBund => "berliner-turn-und-freizeitsport-bund",
        L::LfvMaerkischerTurnerbundBrandenburg => "maerkischer-turnerbund-brandenburg",
        L::LfvBremerTurnverband => "bremer-turnverband",
        L::LfvVerbandFuerTurnenUndFreizeitHamburg => "verband-fuer-turnen-und-freizeit-hamburg",
        L::LfvHessischerTurnverband => "hessischer-turnverband",
        L::LfvTurnverbandMecklenburgVorpommern => "turnverband-mecklenburg-vorpommern",
        L::LfvTurnverbandMittelrhein => "turnverband-mittelrhein",
        L::LfvNiedersaechsischerTurnerBund => "niedersaechsischer-turner-bund",
        L::LfvPfaelzerTurnerbund => "pfaelzer-turnerbund",
        L::LfvRheinhessischerTurnerbund => "rheinhessischer-turnerbund",
        L::LfvRheinischerTurnerbund => "rheinischer-turnerbund",
        L::LfvSaarlaendischerTurnerbund => "saarlaendischer-turnerbund",
        L::LfvSaechsischerTurnVerband => "saechsischer-turn-verband",
        L::LfvLandesturnverbandSachsenAnhalt => "landesturnverband-sachsen-anhalt",
        L::LfvSchleswigHolsteinischerTurnverband => "schleswig-holsteinischer-turnverband",
        L::LfvSchwaebischerTurnerbund => "schwaebischer-turnerbund",
        L::LfvThueringerTurnverband => "thueringer-turnverband",
        L::LfvWestfaelischerTurnerbund => "westfaelischer-turnerbund",
    }
}

fn messages(diagnostics: &EcoVec<SourceDiagnostic>) -> Vec<String> {
    diagnostics.iter().map(|d| d.message.to_string()).collect()
}

#[cfg(test)]
mod pdf_standard_tests {
    use super::{PdfStandard, pdf_standard_conflicts, validate_pdf_standards};

    #[test]
    fn empty_selection_has_no_conflicts() {
        assert!(pdf_standard_conflicts(&[]).is_empty());
    }

    #[test]
    fn a_second_archival_standard_conflicts_and_explains_itself() {
        let conflicts = pdf_standard_conflicts(&[PdfStandard::A2b]);
        for other in [PdfStandard::A3b, PdfStandard::A4] {
            let (_, reason) = conflicts
                .iter()
                .find(|(s, _)| *s == other)
                .unwrap_or_else(|| panic!("{other:?} should conflict with A2b"));
            assert!(reason.contains("PDF/A-Standard"), "reason: {reason}");
        }
    }

    #[test]
    fn pdf_2_0_rules_out_the_1_7_archival_standards_only() {
        let conflicts = pdf_standard_conflicts(&[PdfStandard::V2_0]);
        let flagged: Vec<_> = conflicts.iter().map(|(s, _)| *s).collect();
        assert!(flagged.contains(&PdfStandard::A2b));
        assert!(flagged.contains(&PdfStandard::A3b));
        assert!(!flagged.contains(&PdfStandard::A4));
    }

    #[test]
    fn conflicts_never_contradict_the_matrix_validator() {
        for seed in PdfStandard::ALL {
            for (candidate, _) in pdf_standard_conflicts(&[seed]) {
                assert!(validate_pdf_standards(&[seed, candidate]).is_err());
            }
        }
    }
}
