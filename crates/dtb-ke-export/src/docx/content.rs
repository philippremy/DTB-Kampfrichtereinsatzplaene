//! `DocumentModel` → a WordprocessingML `Body`.
//!
//! This is the DOCX counterpart of `crate::model` for Typst: plain struct
//! construction, every judge name / label / remark handed over as a text value.

use ooxmlsdk::schemas::schemas_openxmlformats_org_wordprocessingml_2006_main as w;

use super::dsl::*;
use super::styles;
use crate::model::{Briefing, DocumentModel, Paragraph as MPara, Row as MRow, Table as MTable};

/// A4 in twips.
const PAGE_W: u64 = 11906;
const PAGE_H: u64 = 16838;
const MARGIN: i64 = 1134; // 2 cm
const MARGIN_BOTTOM: i64 = 720;
/// Content width (page minus both side margins) — the pair table always fills
/// it, split evenly between the (up to) two judging tables.
const CONTENT_W: u64 = PAGE_W - 2 * MARGIN as u64;
/// Role / judge column widths within one table's half — same ~26:74 split
/// `judge-list`'s `(auto, 1fr)` grid produces, in twips.
const ROLE_COL: u64 = 1200;
const JUDGE_COL: u64 = CONTENT_W / 2 - ROLE_COL;

/// The header-strip rule, matching Typst's `stroke: 0.6pt + luma(40%)`.
const RULE_COLOR: &str = "666666";
const RULE_SIZE: u32 = 5; // eighths of a point ≈ 0.6pt

/// Static German boilerplate — the same fixed-form text the Typst template
/// carries (it is part of the form, not the data).
const INTRO: &str = "Liebe Kampfrichter*innen, vielen Dank, dass Ihr Euch für den Einsatz zur \
     Verfügung stellt. Folgende Kampfrichtereinteilung wurde für den Wettkampf vorgenommen:";
const DRESS_CODE: [&str; 5] = [
    "einfarbige schwarze Hose",
    "weiße Bluse mit langem Arm / weißes Hemd mit langem Arm",
    "schwarze Schuhe (keine Stöckelschuhe)",
    "schwarzer Blazer / schwarzes Sakko",
    "langes Haar zu einer ordentlichen Frisur zusammengesteckt",
];
const CLOSING: &str = "Unmittelbar nach Wettkampfende wird eine verpflichtende Nachbesprechung für \
     alle Kampfrichter*innen stattfinden. Bitte berücksichtigt dies in Eurem Zeitplan.";

pub fn build_body(model: &DocumentModel, header_rid: Option<&str>) -> w::Body {
    let mut out: Vec<w::BodyChoice> = Vec::new();
    let p = |para: w::Paragraph| as_body_paragraph(para);

    // ── title block ─────────────────────────────────────────────────────────
    out.push(p(styled_paragraph(styles::TITLE, &model.title)));
    let subtitle = if model.location.is_empty() {
        format!("am {}", model.date)
    } else {
        format!("am {} in {}", model.date, model.location)
    };
    out.push(p(styled_paragraph(styles::SUBTITLE, subtitle)));
    out.push(p(styled_paragraph(styles::INTRO, INTRO)));

    // ── judging tables ─────────────────────────────────────────────────────
    // Up to two tables per row (never mixing qualification with finale — the
    // phases are chunked separately), exactly as `table-rows` groups them in
    // the Typst template. A spacer paragraph sits *between* consecutive pair
    // tables so their `w:tbl` elements are never directly adjacent (Word can
    // visually merge those) — but not after the last one, where a trailing
    // empty paragraph would waste vertical space / nudge the page count.
    let pairs: Vec<&[MTable]> = model
        .phases
        .iter()
        .flat_map(|ph| ph.tables.chunks(2))
        .collect();
    for (i, pair) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(p(spacer()));
        }
        out.push(as_body_table(pair_table(pair)));
    }

    // ── briefing (new page) ────────────────────────────────────────────────
    briefing_section(&model.briefing, &model.date, &mut out);

    // ── free-form remarks ──────────────────────────────────────────────────
    if !model.remarks.is_empty() {
        out.push(p(rule()));
        for para in &model.remarks {
            out.push(p(remark_paragraph(para)));
        }
    }

    w::Body {
        body_choice: out,
        section_properties: Some(Box::new(section_properties(header_rid))),
        ..Default::default()
    }
}

/// One `table-rows` row: up to two judging tables side by side in a single
/// Word table — 4 grid columns (role, judge) × 2. Mirrors the Typst layout:
/// a bordered header strip (label + discipline, stacked, one row each) spans
/// the full width, a short blank row separates it from the data, then one row
/// per judge slot. A half with nothing to show (no second table, or a role
/// this table doesn't have — an STL-M paired with a plain table, say) is one
/// blank cell spanning both its columns rather than two empty cells.
fn pair_table(pair: &[MTable]) -> w::Table {
    let left = &pair[0];
    let right = pair.get(1);

    let mut rows = vec![
        header_row(true, left, right, |t| &t.label, styles::TABLE_LABEL),
        header_row(false, left, right, |t| &t.discipline, styles::DISCIPLINE),
        padding_row(),
    ];

    let row_count = left.rows.len().max(right.map_or(0, |t| t.rows.len()));
    for i in 0..row_count {
        let mut cells = data_half(left.rows.get(i));
        cells.extend(match right {
            Some(t) => data_half(t.rows.get(i)),
            None => vec![blank_half()],
        });
        rows.push(row(cells));
    }

    w::Table {
        table_properties: Some(Box::new(w::TableProperties {
            table_layout: Some(w::TableLayout {
                r#type: Some(w::TableLayoutValues::Fixed),
            }),
            table_width: Some(w::TableWidth {
                width: Some(
                    ooxmlsdk::units::MeasurementOrPercentValue::DecimalNumberOrPercent(
                        ooxmlsdk::units::DecimalNumberOrPercentValue::DecimalNumber(
                            CONTENT_W as i64,
                        ),
                    ),
                ),
                r#type: Some(w::TableWidthUnitValues::Dxa),
            }),
            ..Default::default()
        })),
        table_grid: Some(Box::new(w::TableGrid {
            grid_column: [ROLE_COL, JUDGE_COL, ROLE_COL, JUDGE_COL]
                .map(|w| w::GridColumn {
                    width: Some(twips(w)),
                })
                .to_vec(),
            ..Default::default()
        })),
        table_choice2: rows
            .into_iter()
            .map(|r: w::TableRow| w::TableChoice2::TableRow(Box::new(r)))
            .collect(),
        ..Default::default()
    }
}

/// The label/discipline row: one styled paragraph per half, each spanning
/// that table's 2 (role, judge) columns, bordered on the outer edge of the
/// header strip only (`top`: the label row; the discipline row instead
/// closes the strip on `bottom`; neither has a line between the halves or
/// between label and discipline, matching Typst's single `rect`).
fn header_row(
    top: bool,
    left: &MTable,
    right: Option<&MTable>,
    text: impl Fn(&MTable) -> &str,
    style: &str,
) -> w::TableRow {
    let cell_for = |table: Option<&MTable>, is_left: bool| {
        header_cell(
            header_borders(top, !top, is_left, !is_left),
            table.map(|t| styled_paragraph(style, text(t))),
        )
    };
    row(vec![cell_for(Some(left), true), cell_for(right, false)])
}

fn header_borders(top: bool, bottom: bool, left: bool, right: bool) -> w::TableCellBorders {
    macro_rules! edge {
        ($ty:ident, $flag:expr) => {
            $flag.then(|| w::$ty {
                val: w::BorderValues::Single,
                color: Some(RULE_COLOR.to_string()),
                size: Some(RULE_SIZE),
                space: Some(0),
                ..Default::default()
            })
        };
    }
    w::TableCellBorders {
        top_border: edge!(TopBorder, top),
        bottom_border: edge!(BottomBorder, bottom),
        left_border: edge!(LeftBorder, left),
        right_border: edge!(RightBorder, right),
        ..Default::default()
    }
}

/// A cell spanning 2 grid columns (one table's role+judge width), carrying
/// the given borders and an optional paragraph (`None` → blank, for a
/// missing second table).
fn header_cell(borders: w::TableCellBorders, content: Option<w::Paragraph>) -> w::TableCell {
    w::TableCell {
        table_cell_properties: Some(Box::new(w::TableCellProperties {
            grid_span: Some(w::GridSpan { val: 2 }),
            table_cell_borders: Some(Box::new(borders)),
            table_cell_vertical_alignment: Some(w::TableCellVerticalAlignment {
                val: w::TableVerticalAlignmentValues::Center,
            }),
            ..Default::default()
        })),
        table_cell_choice: vec![w::TableCellChoice::Paragraph(Box::new(
            content.unwrap_or_default(),
        ))],
        ..Default::default()
    }
}

/// A short, borderless, full-width spacer row (Typst's `v(0.2cm)` between the
/// header strip and the judge list).
fn padding_row() -> w::TableRow {
    w::TableRow {
        table_row_properties: Some(Box::new(w::TableRowProperties {
            table_row_properties_choice1: vec![w::TableRowPropertiesChoice::TableRowHeight(
                w::TableRowHeight {
                    val: Some(twips(120)),
                    height_type: Some(w::HeightRuleValues::AtLeast),
                },
            )],
            ..Default::default()
        })),
        table_row_choice: vec![w::TableRowChoice::TableCell(Box::new(cell(
            None,
            Some(4),
            vec![tiny_paragraph()],
        )))],
        ..Default::default()
    }
}

/// A near-zero-height paragraph, so the padding row doesn't grow to a full
/// text line.
fn tiny_paragraph() -> w::Paragraph {
    w::Paragraph {
        paragraph_choice: vec![w::ParagraphChoice::WRun(Box::new(w::Run {
            run_properties: Some(Box::new(w::RunProperties {
                run_properties_choice: vec![w::RunPropertiesChoice::FontSize(w::FontSize {
                    val: hps(4),
                })],
                ..Default::default()
            })),
            ..Default::default()
        }))],
        ..Default::default()
    }
}

/// One judge row for one table half: `role:` / judge name, plain text (Typst
/// doesn't bold or shade these either) — or one blank cell spanning both
/// columns when this table has no such row.
fn data_half(row: Option<&MRow>) -> Vec<w::TableCell> {
    match row {
        Some(r) => vec![
            cell(None, None, vec![body_text(format!("{}:", r.role))]),
            cell(None, None, vec![body_text(&r.judge)]),
        ],
        None => vec![blank_half()],
    }
}

fn blank_half() -> w::TableCell {
    cell(None, Some(2), vec![spacer()])
}

fn briefing_section(b: &Briefing, date: &str, out: &mut Vec<w::BodyChoice>) {
    let p = |para: w::Paragraph| as_body_paragraph(para);

    // Start the briefing on a fresh page. `w:pageBreakBefore` on the first
    // paragraph rather than a standalone page-break run — the latter left an
    // empty page behind it in LibreOffice.
    // The sentence wraps to a second line after the time clause.
    let mut opener = paragraph(
        None,
        vec![
            styled_text(
                format!(
                    "Die Kampfrichterbesprechung findet am {date} {}",
                    b.time_clause
                ),
                true,
                false,
                false,
                None,
            ),
            line_break(),
            styled_text("in Kampfrichterkleidung statt.", true, false, false, None),
        ],
    );
    opener
        .paragraph_properties
        .get_or_insert_with(Default::default)
        .page_break_before = Some(w::PageBreakBefore::default());
    out.push(p(opener));
    out.push(p(spacer()));

    if b.spare.is_empty() {
        out.push(p(body_text("Ersatzkampfrichter*innen: --")));
    } else {
        for g in &b.spare {
            let label = g
                .label
                .as_deref()
                .map(|l| format!(" ({l})"))
                .unwrap_or_default();
            out.push(p(body_text(format!(
                "Ersatzkampfrichter*innen{label}: {}",
                g.names.join(", ")
            ))));
        }
    }
    let responsible = if b.responsible.is_empty() {
        "--".to_string()
    } else {
        b.responsible.join(", ")
    };
    out.push(p(body_text(format!(
        "Kampfrichterverantwortliche*r: {responsible}"
    ))));

    out.push(p(styled_paragraph(
        styles::SECTION_LABEL,
        "Kampfrichterkleidung:",
    )));
    for item in DRESS_CODE {
        // The "–" marker is supplied by the `KEBullet` style's list definition.
        out.push(p(paragraph(Some(styles::BULLET), vec![text(item)])));
    }
    out.push(p(spacer()));
    out.push(p(body_text(CLOSING)));
}

/// A remark paragraph from styled runs.
fn remark_paragraph(para: &MPara) -> w::Paragraph {
    let runs = para
        .runs
        .iter()
        .map(|r| styled_text(&r.text, r.bold, r.italic, r.underline, r.color.as_deref()))
        .collect();
    paragraph(None, runs)
}

// ── small element helpers specific to this document ────────────────────────

/// A thin horizontal rule (a paragraph with just a bottom border).
fn rule() -> w::Paragraph {
    w::Paragraph {
        paragraph_properties: Some(Box::new(w::ParagraphProperties {
            spacing_between_lines: Some(w::SpacingBetweenLines {
                before: Some(stwips(360)),
                after: Some(stwips(160)),
                ..Default::default()
            }),
            paragraph_borders: Some(Box::new(w::ParagraphBorders {
                bottom_border: Some(w::BottomBorder {
                    val: w::BorderValues::Single,
                    color: Some("999999".to_string()),
                    size: Some(4),
                    space: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// The top margin when a header carries the logo strip — roomy enough that the
/// swoosh (4 cm tall, ~0.5 cm down) clears the page-one title, echoing the
/// Typst template's generous `margin.top`. Without a header it stays at the
/// plain 2 cm.
const MARGIN_TOP_WITH_HEADER: i64 = 2205;

fn section_properties(header_rid: Option<&str>) -> w::SectionProperties {
    let top = if header_rid.is_some() {
        MARGIN_TOP_WITH_HEADER
    } else {
        MARGIN
    };
    w::SectionProperties {
        section_properties_choice: header_rid
            .map(|id| {
                vec![w::SectionPropertiesChoice::HeaderReference(
                    w::HeaderReference {
                        r#type: w::HeaderFooterValues::Default,
                        id: id.to_string(),
                        ..Default::default()
                    },
                )]
            })
            .unwrap_or_default(),
        page_size: Some(w::PageSize {
            width: Some(twips(PAGE_W)),
            height: Some(twips(PAGE_H)),
            ..Default::default()
        }),
        page_margin: Some(w::PageMargin {
            top: Some(stwips(top)),
            bottom: Some(stwips(MARGIN_BOTTOM)),
            left: Some(twips(MARGIN as u64)),
            right: Some(twips(MARGIN as u64)),
            header: Some(twips(340)),
            footer: Some(twips(720)),
            gutter: Some(twips(0)),
            ..Default::default()
        }),
        ..Default::default()
    }
}
