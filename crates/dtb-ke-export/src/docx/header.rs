//! The page header part — the two logos that repeat on every page: the org
//! emblem top-left, the "TURNEN!" swoosh top-right.
//!
//! Both logos are **floating pictures anchored to the page** (`wp:anchor`,
//! `behindDoc`, `wrapNone`) so they can bleed past the text margins toward the
//! page edges, as the Typst template's `place()`d header art does. They do not
//! affect text flow, so `super::content` sets a generous top margin to keep the
//! page-one title clear of the swoosh. The emblem is sized by orientation
//! (landscape → width-capped, portrait → height-capped), same rule as the Typst
//! template, and is centred *vertically* on the swoosh's midline. Each logo is a
//! PNG blip with an `asvg:svgBlip` extension (see [`super::image`]); the PNG
//! comes from [`super::render`].

use ooxmlsdk::parts::header_part::HeaderPart;
use ooxmlsdk::parts::image_part::ImagePart;
use ooxmlsdk::parts::main_document_part::MainDocumentPart;
use ooxmlsdk::parts::wordprocessing_document::WordprocessingDocument;
use ooxmlsdk::schemas::schemas_openxmlformats_org_wordprocessingml_2006_main as w;

use super::image::{LogoPlacement, anchored_logo_run};
use super::render::{self, RenderedLogo, SizePolicy};
use crate::ExportError;

const HEADER_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml";

/// Relationship id used for the header from `document.xml.rels`. Not an
/// `rIdN` (those are auto-assigned to styles / numbering / fonts), so it never
/// collides.
const HEADER_RID: &str = "rIdHdr1";

/// EMU per cm, and the A4 page width in EMU (used for the right-hand offset).
const CM: i64 = 360_000;
const PAGE_W_EMU: i64 = 21 * CM;

/// Emblem sizing (EMU) — the caps come from `crate::svg` so the DOCX header and
/// the Typst template stay in step (`render::rasterise` applies the same
/// `svg::emblem_frame` rule internally).
const EMBLEM_LANDSCAPE_W: i64 = (crate::svg::EMBLEM_LANDSCAPE_W_CM * CM as f64) as i64;
const EMBLEM_LANDSCAPE_H: i64 = (crate::svg::EMBLEM_LANDSCAPE_H_CM * CM as f64) as i64;
const EMBLEM_PORTRAIT_H: i64 = (crate::svg::EMBLEM_PORTRAIT_H_CM * CM as f64) as i64;

/// The swoosh fits inside a 4 × 4 cm box (it is close to square).
const SWOOSH_BOX: i64 = (3.5 * CM as f32) as i64;

/// Placement from the **page** top-left corner. The emblem's left edge and the
/// swoosh's right edge sit *inside* the 2 cm text margin so both bleed toward
/// the page edges; the emblem is centred *vertically* on the swoosh's midline
/// (its `offset_y` is derived from its height).
const EMBLEM_X: i32 = (20 * CM / 10) as i32; // 1.1 cm from the left page edge
const SWOOSH_RIGHT_GAP: i32 = (50 * CM / 100) as i32; // 0.35 cm from the right page edge
const SWOOSH_Y: i32 = (75 * CM / 100) as i32; // 0.5 cm from the top
/// The vertical centre the emblem is aligned to (middle of the swoosh box).
const EMBLEM_CENTER_Y: i32 = SWOOSH_Y + (SWOOSH_BOX / 2) as i32;

/// The source SVG bytes of the two header logos.
pub struct HeaderLogos<'a> {
    pub org: Option<&'a [u8]>,
    pub turnen: Option<&'a [u8]>,
}

impl HeaderLogos<'_> {
    fn is_empty(&self) -> bool {
        self.org.is_none() && self.turnen.is_none()
    }
}

/// Build the header part (image parts + `header1.xml`) and return the
/// relationship id the section properties must reference. `Ok(None)` when
/// there is no artwork to place.
pub fn build(
    docx: &mut WordprocessingDocument,
    main: &MainDocumentPart,
    logos: &HeaderLogos<'_>,
) -> Result<Option<String>, ExportError> {
    if logos.is_empty() {
        return Ok(None);
    }

    let org = logos
        .org
        .map(|b| {
            render::rasterise(
                b,
                SizePolicy::ByOrientation {
                    landscape_w: EMBLEM_LANDSCAPE_W,
                    landscape_h: EMBLEM_LANDSCAPE_H,
                    portrait_h: EMBLEM_PORTRAIT_H,
                },
            )
        })
        .transpose()?
        .flatten();
    let turnen = logos
        .turnen
        .map(|b| {
            render::rasterise(
                b,
                SizePolicy::Contain {
                    w: SWOOSH_BOX,
                    h: SWOOSH_BOX,
                },
            )
        })
        .transpose()?
        .flatten();
    if org.is_none() && turnen.is_none() {
        return Ok(None);
    }

    let header: HeaderPart = main.add_new_part_with_content_type_and_extension(
        docx,
        HEADER_RID,
        HEADER_CONTENT_TYPE,
        "xml",
    )?;

    // Image parts live under the header; ids are local to header1.xml.rels.
    let left = match org {
        Some(logo) => Some(embed_logo(docx, &header, &logo, 1)?),
        None => None,
    };
    let right = match turnen {
        Some(logo) => Some(embed_logo(docx, &header, &logo, 2)?),
        None => None,
    };

    header.set_root_element(
        docx,
        w::Header {
            xmlns: namespaces(),
            header_choice: vec![w::HeaderChoice::Paragraph(Box::new(header_paragraph(
                left.as_ref(),
                right.as_ref(),
            )))],
            ..Default::default()
        },
    )?;

    Ok(Some(HEADER_RID.to_string()))
}

/// One embedded logo: its PNG + SVG parts and the frame size, ready to hand to
/// [`anchored_logo_run`]. `slot` (1 = left, 2 = right) keeps relationship ids
/// and drawing ids distinct.
struct EmbeddedLogo {
    png_rid: String,
    svg_rid: String,
    cx: i64,
    cy: i64,
    slot: u32,
    name: &'static str,
}

fn embed_logo(
    docx: &mut WordprocessingDocument,
    header: &HeaderPart,
    logo: &RenderedLogo,
    slot: u32,
) -> Result<EmbeddedLogo, ExportError> {
    let png_rid = format!("rIdImg{slot}");
    let svg_rid = format!("rIdSvg{slot}");

    let png_part: ImagePart = header.add_new_part_with_content_type_and_extension(
        docx,
        png_rid.clone(),
        "image/png",
        "png",
    )?;
    png_part.set_data(docx, logo.png.clone())?;

    let svg_part: ImagePart = header.add_new_part_with_content_type_and_extension(
        docx,
        svg_rid.clone(),
        "image/svg+xml",
        "svg",
    )?;
    svg_part.set_data(docx, logo.svg.clone())?;

    Ok(EmbeddedLogo {
        png_rid,
        svg_rid,
        cx: logo.cx,
        cy: logo.cy,
        slot,
        name: if slot == 1 { "Verbandslogo" } else { "TURNEN!" },
    })
}

/// The header's single (near-empty) paragraph. Word requires a header to hold
/// at least one block; the two logo anchors ride on it. A 2 pt run keeps the
/// paragraph's own line from adding height.
fn header_paragraph(left: Option<&EmbeddedLogo>, right: Option<&EmbeddedLogo>) -> w::Paragraph {
    let mut runs: Vec<w::Run> = Vec::new();

    if let Some(l) = left {
        runs.push(anchored_logo_run(&LogoPlacement {
            png_rid: &l.png_rid,
            svg_rid: &l.svg_rid,
            cx: l.cx,
            cy: l.cy,
            offset_x: EMBLEM_X,
            offset_y: EMBLEM_CENTER_Y - (l.cy / 2) as i32,
            id: l.slot,
            name: l.name,
            z: 251_658_240,
        }));
    }
    if let Some(r) = right {
        runs.push(anchored_logo_run(&LogoPlacement {
            png_rid: &r.png_rid,
            svg_rid: &r.svg_rid,
            cx: r.cx,
            cy: r.cy,
            offset_x: (PAGE_W_EMU as i32) - SWOOSH_RIGHT_GAP - (r.cx as i32),
            offset_y: SWOOSH_Y,
            id: r.slot,
            name: r.name,
            z: 251_659_264,
        }));
    }

    // a tiny trailing run so the anchors have a run to hang on even if both
    // logos are absent (they aren't here, but it keeps the paragraph valid)
    runs.push(w::Run {
        run_properties: Some(Box::new(w::RunProperties {
            run_properties_choice: vec![w::RunPropertiesChoice::FontSize(w::FontSize {
                val: ooxmlsdk::units::HpsMeasureValue::HalfPoints(4),
            })],
            ..Default::default()
        })),
        ..Default::default()
    });

    w::Paragraph {
        paragraph_properties: Some(Box::new(w::ParagraphProperties {
            spacing_between_lines: Some(w::SpacingBetweenLines {
                before: Some(ooxmlsdk::units::SignedTwipsMeasureValue::Twips(0)),
                after: Some(ooxmlsdk::units::SignedTwipsMeasureValue::Twips(0)),
                line: Some(ooxmlsdk::units::SignedTwipsMeasureValue::Twips(20)),
                line_rule: Some(w::LineSpacingRuleValues::Exact),
                ..Default::default()
            }),
            ..Default::default()
        })),
        paragraph_choice: runs
            .into_iter()
            .map(|r| w::ParagraphChoice::WRun(Box::new(r)))
            .collect(),
        ..Default::default()
    }
}

fn namespaces() -> Vec<ooxmlsdk::common::XmlNamespace> {
    use ooxmlsdk::common::XmlNamespace;
    use ooxmlsdk::namespaces::XmlKnownNamespace as N;
    [N::W, N::R, N::A, N::Pic, N::Wp, N::Asvg]
        .into_iter()
        .map(XmlNamespace::known)
        .collect()
}
