//! The `styles.xml` part: named styles the body references, so the document is
//! restyleable in Word and the markup stays small.

use ooxmlsdk::schemas::schemas_openxmlformats_org_wordprocessingml_2006_main as w;

use super::dsl::{hps, stwips};
use ooxmlsdk::simple_type::OnOffValue;

/// The document font (falls back to a system serif/sans if not present /
/// not embedded).
pub const FONT: &str = "Archivo";

/// The title font — the condensed extra-bold cut (matches the Typst template).
/// Its `w:font` family name (name-ID 1 of `Archivo_Condensed-ExtraBold.ttf`).
pub const TITLE_FONT: &str = "Archivo Condensed ExtraBold";

/// Style ids referenced from [`super::content`].
pub const NORMAL: &str = "Normal";
pub const TITLE: &str = "KETitle";
pub const SUBTITLE: &str = "KESubtitle";
pub const INTRO: &str = "KEIntro";
pub const TABLE_LABEL: &str = "KETableLabel";
pub const DISCIPLINE: &str = "KEDiscipline";
pub const SECTION_LABEL: &str = "KESectionLabel";
pub const BULLET: &str = "KEBullet";

/// `w:numId` of the bullet-list definition in `numbering.xml` that `KEBullet`
/// references (see [`super::numbering`]).
pub const BULLET_NUM_ID: i32 = 1;

fn run_fonts_named(name: &str) -> w::RunFonts {
    w::RunFonts {
        ascii: Some(name.to_string()),
        high_ansi: Some(name.to_string()),
        complex_script: Some(name.to_string()),
        ..Default::default()
    }
}

fn run_fonts() -> w::RunFonts {
    run_fonts_named(FONT)
}

fn sz(half_points: u64) -> w::FontSize {
    w::FontSize {
        val: hps(half_points),
    }
}

fn spacing(before: i64, after: i64) -> w::SpacingBetweenLines {
    w::SpacingBetweenLines {
        before: Some(stwips(before)),
        after: Some(stwips(after)),
        ..Default::default()
    }
}

fn jc(v: w::JustificationValues) -> w::Justification {
    w::Justification { val: v }
}

fn style(
    id: &str,
    name: &str,
    based_on: Option<&str>,
    ppr: Option<w::StyleParagraphProperties>,
    rpr: Option<w::StyleRunProperties>,
) -> w::Style {
    w::Style {
        r#type: Some(w::StyleValues::Paragraph),
        style_id: Some(id.to_string()),
        custom_style: Some(OnOffValue::from(true)),
        style_name: Some(w::StyleName {
            val: name.to_string(),
        }),
        based_on: based_on.map(|b| w::BasedOn { val: b.to_string() }),
        next_paragraph_style: Some(w::NextParagraphStyle {
            val: NORMAL.to_string(),
        }),
        primary_style: Some(w::PrimaryStyle::default()),
        style_paragraph_properties: ppr.map(Box::new),
        style_run_properties: rpr.map(Box::new),
        ..Default::default()
    }
}

pub fn build() -> w::Styles {
    let mut styles = Vec::new();

    // Normal — the document default, marked w:default.
    styles.push(w::Style {
        r#type: Some(w::StyleValues::Paragraph),
        style_id: Some(NORMAL.to_string()),
        default: Some(OnOffValue::from(true)),
        style_name: Some(w::StyleName {
            val: "Normal".to_string(),
        }),
        style_paragraph_properties: Some(Box::new(w::StyleParagraphProperties {
            spacing_between_lines: Some(spacing(60, 60)),
            ..Default::default()
        })),
        style_run_properties: Some(Box::new(w::StyleRunProperties {
            run_fonts: Some(run_fonts()),
            font_size: Some(sz(21)),
            ..Default::default()
        })),
        ..Default::default()
    });

    styles.push(style(
        TITLE,
        "DTB-KE Titel",
        Some(NORMAL),
        Some(w::StyleParagraphProperties {
            justification: Some(jc(w::JustificationValues::Center)),
            spacing_between_lines: Some(spacing(0, 80)),
            keep_next: Some(w::KeepNext::default()),
            ..Default::default()
        }),
        Some(w::StyleRunProperties {
            run_fonts: Some(run_fonts_named(TITLE_FONT)),
            bold: Some(w::Bold::default()),
            font_size: Some(sz(40)),
            ..Default::default()
        }),
    ));

    styles.push(style(
        SUBTITLE,
        "DTB-KE Untertitel",
        Some(NORMAL),
        Some(w::StyleParagraphProperties {
            justification: Some(jc(w::JustificationValues::Center)),
            spacing_between_lines: Some(spacing(0, 200)),
            ..Default::default()
        }),
        Some(w::StyleRunProperties {
            bold: Some(w::Bold::default()),
            italic: Some(w::Italic::default()),
            font_size: Some(sz(28)),
            ..Default::default()
        }),
    ));

    styles.push(style(
        INTRO,
        "DTB-KE Einleitung",
        Some(NORMAL),
        Some(w::StyleParagraphProperties {
            spacing_between_lines: Some(spacing(0, 300)),
            justification: Some(w::Justification {
                val: w::JustificationValues::Center,
            }),
            ..Default::default()
        }),
        None,
    ));

    styles.push(style(
        TABLE_LABEL,
        "DTB-KE Kampfgerichtbezeichnung",
        Some(NORMAL),
        Some(w::StyleParagraphProperties {
            spacing_between_lines: Some(spacing(60, 60)),
            keep_next: Some(w::KeepNext::default()),
            ..Default::default()
        }),
        Some(w::StyleRunProperties {
            bold: Some(w::Bold::default()),
            font_size: Some(sz(23)),
            ..Default::default()
        }),
    ));

    styles.push(style(
        DISCIPLINE,
        "DTB-KE Kampfgerichtdisziplin",
        Some(NORMAL),
        Some(w::StyleParagraphProperties {
            spacing_between_lines: Some(spacing(40, 40)),
            keep_next: Some(w::KeepNext::default()),
            ..Default::default()
        }),
        Some(w::StyleRunProperties {
            italic: Some(w::Italic::default()),
            font_size: Some(sz(19)),
            color: Some(w::Color {
                val: Some("4D4D4D".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }),
    ));

    styles.push(style(
        SECTION_LABEL,
        "DTB-KE Abschnitt",
        Some(NORMAL),
        Some(w::StyleParagraphProperties {
            spacing_between_lines: Some(spacing(240, 80)),
            keep_next: Some(w::KeepNext::default()),
            ..Default::default()
        }),
        Some(w::StyleRunProperties {
            underline: Some(w::Underline {
                val: Some(w::UnderlineValues::Single),
                ..Default::default()
            }),
            ..Default::default()
        }),
    ));

    styles.push(style(
        BULLET,
        "DTB-KE Liste",
        Some(NORMAL),
        Some(w::StyleParagraphProperties {
            spacing_between_lines: Some(spacing(0, 40)),
            // A real list: the "–" markers come from numbering.xml, not text.
            numbering_properties: Some(Box::new(w::NumberingProperties {
                numbering_level_reference: Some(w::NumberingLevelReference { val: 0 }),
                numbering_id: Some(w::NumberingId { val: BULLET_NUM_ID }),
                ..Default::default()
            })),
            indentation: Some(w::Indentation {
                start_characters: Some(200),
                hanging: Some(stwips(340)),
                ..Default::default()
            }),
            ..Default::default()
        }),
        None,
    ));

    w::Styles {
        style: styles,
        ..Default::default()
    }
}
