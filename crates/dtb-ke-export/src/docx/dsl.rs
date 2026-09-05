//! Thin constructors over the `ooxmlsdk` WordprocessingML types.
//!
//! The generated schema types are 1:1 with ECMA-376 and therefore verbose
//! (every element carries all its optional attributes). These helpers keep
//! [`super::content`] readable — building a `Run`/`Paragraph`/table cell is a
//! one-liner, the same way the .NET SDK's element constructors do.

use ooxmlsdk::schemas::schemas_openxmlformats_org_wordprocessingml_2006_main as w;
use ooxmlsdk::schemas::xml::SpaceProcessingModeValues;
use ooxmlsdk::units::{HpsMeasureValue, SignedTwipsMeasureValue, TwipsMeasureValue};

/// Half-points, for `w:sz`.
pub fn hps(half_points: u64) -> HpsMeasureValue {
    HpsMeasureValue::HalfPoints(half_points)
}
/// Twips (1/20 pt), for widths / positive measures.
pub fn twips(t: u64) -> TwipsMeasureValue {
    TwipsMeasureValue::Twips(t)
}
/// Signed twips, for margins / spacing.
pub fn stwips(t: i64) -> SignedTwipsMeasureValue {
    SignedTwipsMeasureValue::Twips(t)
}

/// A `w:t` run of plain text (`xml:space="preserve"` so surrounding whitespace
/// survives).
pub fn text(s: impl Into<String>) -> w::Run {
    w::Run {
        run_choice: vec![w::RunChoice::Text(w::Text(w::TextType {
            space: Some(SpaceProcessingModeValues::Preserve),
            xml_content: Some(s.into()),
        }))],
        ..Default::default()
    }
}

/// A run carrying direct bold / italic / underline / color character
/// formatting. `color` is `#RRGGBB` (the leading `#` is stripped — the OOXML
/// `w:color/@w:val` attribute is bare hex).
pub fn styled_text(
    s: impl Into<String>,
    bold: bool,
    italic: bool,
    underline: bool,
    color: Option<&str>,
) -> w::Run {
    let mut props = Vec::new();
    if bold {
        props.push(w::RunPropertiesChoice::Bold(w::Bold::default()));
    }
    if italic {
        props.push(w::RunPropertiesChoice::Italic(w::Italic::default()));
    }
    if underline {
        props.push(w::RunPropertiesChoice::Underline(Box::new(w::Underline {
            val: Some(w::UnderlineValues::Single),
            ..Default::default()
        })));
    }
    if let Some(color) = color {
        props.push(w::RunPropertiesChoice::Color(Box::new(w::Color {
            val: Some(color.trim_start_matches('#').to_owned()),
            ..Default::default()
        })));
    }
    let mut run = text(s);
    if !props.is_empty() {
        run.run_properties = Some(Box::new(w::RunProperties {
            run_properties_choice: props,
            ..Default::default()
        }));
    }
    run
}

/// A paragraph of the given runs, optionally with a named style.
pub fn paragraph(style: Option<&str>, runs: Vec<w::Run>) -> w::Paragraph {
    let props = style.map(|id| {
        Box::new(w::ParagraphProperties {
            paragraph_style_id: Some(w::ParagraphStyleId {
                val: id.to_string(),
            }),
            ..Default::default()
        })
    });
    w::Paragraph {
        paragraph_properties: props,
        paragraph_choice: runs
            .into_iter()
            .map(|r| w::ParagraphChoice::WRun(Box::new(r)))
            .collect(),
        ..Default::default()
    }
}

/// A styled paragraph with one plain-text run.
pub fn styled_paragraph(style: &str, s: impl Into<String>) -> w::Paragraph {
    paragraph(Some(style), vec![text(s)])
}

/// A normal-body paragraph with one plain-text run.
pub fn body_text(s: impl Into<String>) -> w::Paragraph {
    paragraph(None, vec![text(s)])
}

/// An empty paragraph — vertical whitespace.
pub fn spacer() -> w::Paragraph {
    w::Paragraph::default()
}

/// A `<w:br/>` run — a line break within a paragraph.
pub fn line_break() -> w::Run {
    w::Run {
        run_choice: vec![w::RunChoice::Break(w::Break::default())],
        ..Default::default()
    }
}

pub fn as_body_paragraph(p: w::Paragraph) -> w::BodyChoice {
    w::BodyChoice::Paragraph(Box::new(p))
}
pub fn as_body_table(t: w::Table) -> w::BodyChoice {
    w::BodyChoice::Table(Box::new(t))
}

/// A table cell holding block content, with an optional shading fill
/// (hex `RRGGBB`, no `#`) and an optional `w:gridSpan`. Always vertically
/// centered — every table in this document wants that.
pub fn cell(fill: Option<&str>, span: Option<u32>, content: Vec<w::Paragraph>) -> w::TableCell {
    let mut props = w::TableCellProperties {
        table_cell_vertical_alignment: Some(w::TableCellVerticalAlignment {
            val: w::TableVerticalAlignmentValues::Center,
        }),
        ..Default::default()
    };
    if let Some(hex) = fill {
        props.shading = Some(w::Shading {
            val: Some(w::ShadingPatternValues::Clear),
            color: Some("auto".to_string()),
            fill: Some(hex.to_string()),
            ..Default::default()
        });
    }
    if let Some(n) = span {
        props.grid_span = Some(w::GridSpan { val: n as i32 });
    }
    w::TableCell {
        table_cell_properties: Some(Box::new(props)),
        table_cell_choice: content
            .into_iter()
            .map(|p| w::TableCellChoice::Paragraph(Box::new(p)))
            .collect(),
        ..Default::default()
    }
}

pub fn row(cells: Vec<w::TableCell>) -> w::TableRow {
    w::TableRow {
        table_row_choice: cells
            .into_iter()
            .map(|c| w::TableRowChoice::TableCell(Box::new(c)))
            .collect(),
        ..Default::default()
    }
}
