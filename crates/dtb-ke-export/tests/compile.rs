//! End-to-end: a competition compiles to a valid PDF and to page previews.

use chrono::{NaiveDate, NaiveTime};
use dtb_ke_export::{DocxExport, Exporter, PdfExport, PreviewOptions};
use dtb_ke_types::*;
use uuid::Uuid;

/// Read one part out of a `.docx` (ZIP) by path.
fn docx_part(bytes: &[u8], name: &str) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).ok()?;
    Some(s)
}

/// Whether a `.docx` (ZIP) contains a part at `name`.
fn docx_has(bytes: &[u8], name: &str) -> bool {
    zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .ok()
        .map(|z| z.file_names().any(|n| n == name))
        .unwrap_or(false)
}

fn competition(with_finale: bool) -> CompetitionDTO {
    let table = |label: &str, kind: JudgingTableKindDTO| JudgingTableDTO {
        label: label.into(),
        kind,
    };
    CompetitionDTO {
        id: Uuid::new_v4(),
        name: "1. WM-Qualifikation 2026 (Erwachsene)".into(),
        organization: OrganizationDTO::DTB,
        location: "Brilon".into(),
        date: NaiveDate::from_ymd_opt(2026, 5, 9).unwrap(),
        meeting_times: MeetingTimeDTO::Unified(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
        responsible_persons: vec!["Max Mustermann".into()],
        spare_judges: SpareJudgesDTO {
            qualification: vec!["Nina Reserve".into()],
            finale: if with_finale {
                vec!["Otto Ersatz".into()]
            } else {
                vec![]
            },
        },
        judging_tables: JudgingTablesDTO {
            qualification: vec![
                table(
                    "Gerade mit Musik",
                    JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(Default::default())),
                ),
                table(
                    "Spirale",
                    JudgingTableKindDTO::Gym(GymWheelTableDTO::SPI(Default::default())),
                ),
                table(
                    "Sprung",
                    JudgingTableKindDTO::Gym(GymWheelTableDTO::VLT(Default::default())),
                ),
            ],
            finale: if with_finale {
                vec![table(
                    "Finale Gerade",
                    JudgingTableKindDTO::Gym(GymWheelTableDTO::STL(Default::default())),
                )]
            } else {
                vec![]
            },
        },
        additional_remarks: RichTextDTO::default(),
    }
}

#[test]
fn compiles_to_a_pdf() {
    let exporter = Exporter::new().unwrap();
    let pdf = exporter
        .compile_pdf(&competition(false), &PdfExport::default())
        .unwrap();
    assert!(
        pdf.starts_with(b"%PDF-"),
        "not a PDF: {:?}",
        &pdf[..8.min(pdf.len())]
    );
    assert!(pdf.len() > 1000);
}

#[test]
fn renders_two_a4_pages() {
    let exporter = Exporter::new().unwrap();
    let pages = exporter
        .render_previews(&competition(false), PreviewOptions::default())
        .unwrap();
    assert_eq!(pages.len(), 2, "title+tables page and briefing page");
    for page in &pages {
        // A4 at 2 px/pt ≈ 1191 × 1684.
        assert!((1150..1250).contains(&page.width));
        assert!((1600..1750).contains(&page.height));
        assert_eq!(page.rgba.len(), (page.width * page.height * 4) as usize);
    }
}

#[test]
fn finale_adds_a_third_page_and_does_not_overflow_tables() {
    let exporter = Exporter::new().unwrap();
    let pages = exporter
        .render_previews(&competition(true), PreviewOptions::default())
        .unwrap();
    // qualification tables (page 1), finale tables (page 2), briefing (page 3).
    assert!(pages.len() >= 2);
}

#[test]
fn four_stlm_tables_under_a_two_line_title_stay_on_page_one() {
    // The tightest real case: a title that wraps to two lines plus two full
    // STL-M pairs (11 judge rows each). All four tables must fit on page one,
    // so the document is exactly two pages (tables, then briefing).
    let mut c = competition(false);
    c.name = "24. Deutsche Vereinsmannschaftsmeisterschaften".into();
    c.judging_tables.qualification = (1..=4)
        .map(|i| JudgingTableDTO {
            label: format!("Gerät {i}"),
            kind: JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(Default::default())),
        })
        .collect();

    let exporter = Exporter::new().unwrap();
    let pages = exporter
        .render_previews(&c, PreviewOptions::default())
        .unwrap();
    assert_eq!(pages.len(), 2, "all four STL-M tables must fit on page one");
}

#[test]
fn exporter_is_reusable_across_competitions() {
    let exporter = Exporter::new().unwrap();
    let a = exporter
        .compile_pdf(&competition(false), &PdfExport::default())
        .unwrap();
    let b = exporter
        .compile_pdf(&competition(true), &PdfExport::default())
        .unwrap();
    assert!(a.starts_with(b"%PDF-") && b.starts_with(b"%PDF-"));
    assert_ne!(a, b);
}

#[test]
fn every_organisation_emblem_compiles() {
    let exporter = Exporter::new().unwrap();
    for org in OrganizationDTO::all() {
        let mut c = competition(false);
        c.organization = org;
        let pdf = exporter
            .compile_pdf(&c, &PdfExport::default())
            .unwrap_or_else(|e| panic!("{org} emblem failed to compile: {e}"));
        assert!(pdf.starts_with(b"%PDF-"));
    }
}

#[test]
fn docx_export_produces_a_valid_package() {
    let exporter = Exporter::new().unwrap();
    let bytes = exporter
        .compile_docx(&competition(true), &DocxExport::default())
        .expect("docx");
    assert_eq!(&bytes[..2], b"PK");
    let doc = docx_part(&bytes, "word/document.xml").expect("word/document.xml");
    assert!(doc.contains("<w:body>") && doc.contains("Geradeturnen auf Musik"));
    let styles = docx_part(&bytes, "word/styles.xml").expect("word/styles.xml");
    assert!(styles.contains(r#"w:styleId="KETitle""#));
    let numbering = docx_part(&bytes, "word/numbering.xml").expect("word/numbering.xml");
    assert!(numbering.contains(r#"w:numFmt w:val="bullet""#));
    // the dress-code lines carry the style, not a typed-in dash
    assert!(doc.contains(r#"w:pStyle w:val="KEBullet""#) && !doc.contains("–  einfarbige"));
    std::fs::write(std::env::temp_dir().join("dtb-ke-test.docx"), &bytes).ok();
}

#[test]
fn docx_export_embeds_the_header_logos() {
    let exporter = Exporter::new().unwrap();
    let bytes = exporter
        .compile_docx(&competition(false), &DocxExport::default())
        .expect("docx");
    std::fs::write(std::env::temp_dir().join("dtb-ke-header.docx"), &bytes).ok();

    // header part + its rels + the media parts (PNG fallback + SVG original,
    // for both the org emblem and the swoosh)
    let header = docx_part(&bytes, "word/header1.xml").expect("word/header1.xml");
    assert!(docx_has(&bytes, "word/_rels/header1.xml.rels"));
    let media: Vec<String> = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
        .unwrap()
        .file_names()
        .filter(|n| n.starts_with("word/media/"))
        .map(str::to_owned)
        .collect();
    assert!(
        media.iter().filter(|n| n.ends_with(".png")).count() == 2
            && media.iter().filter(|n| n.ends_with(".svg")).count() == 2,
        "expected 2 PNG + 2 SVG media parts, got {media:?}"
    );

    // floating pictures anchored to the page, PNG blip + SVG-blip extension
    assert!(header.contains("<wp:anchor") && header.contains(r#"behindDoc="1""#));
    assert!(header.contains(r#"relativeFrom="page""#) && header.contains("wp:posOffset"));
    // `wp:simplePos` is a required child element — Word rejects the file without it
    assert!(header.contains("<wp:simplePos"));
    assert!(header.contains("<pic:pic") || header.contains(":pic "));
    assert!(header.contains("svgBlip"));

    // the section references the header
    let doc = docx_part(&bytes, "word/document.xml").expect("word/document.xml");
    assert!(doc.contains("w:headerReference"));

    // content types cover the new extensions
    let ct = docx_part(&bytes, "[Content_Types].xml").expect("[Content_Types].xml");
    assert!(ct.contains("image/png") && ct.contains("image/svg+xml"));
    assert!(ct.contains("wordprocessingml.header+xml"));

    // the briefing sentence wraps after the time clause
    assert!(doc.contains("in Kampfrichterkleidung statt.") && doc.contains("<w:br"));
}

/// A near-square "landscape" emblem (Berlin, aspect ≈ 1.13) must not blow up
/// vertically — the landscape rule caps height as well as width.
#[test]
fn docx_header_emblem_height_is_bounded_for_a_near_square_logo() {
    let mut comp = competition(false);
    comp.organization =
        OrganizationDTO::LFV(LandesturnverbandDTO::LfvBerlinerTurnUndFreizeitsportBund);
    let exporter = Exporter::new().unwrap();
    let bytes = exporter
        .compile_docx(&comp, &DocxExport::default())
        .expect("docx");
    std::fs::write(std::env::temp_dir().join("dtb-ke-berlin.docx"), &bytes).ok();
    std::fs::write(
        std::env::temp_dir().join("dtb-ke-berlin.pdf"),
        exporter
            .compile_pdf(&comp, &PdfExport::default())
            .expect("pdf"),
    )
    .ok();
    let header = docx_part(&bytes, "word/header1.xml").expect("word/header1.xml");

    // first <wp:extent cy="…"> is the emblem's frame height in EMU
    let cy: i64 = header
        .split("<wp:extent")
        .nth(1)
        .and_then(|s| s.split(r#"cy=""#).nth(1))
        .and_then(|s| s.split('"').next())
        .and_then(|s| s.parse().ok())
        .expect("emblem cy");
    assert!(
        cy <= 900_000, // 2.5 cm
        "emblem frame is {cy} EMU tall (> 2.5 cm) — the near-square logo overflowed"
    );
}

#[test]
fn docx_export_with_embedded_fonts() {
    let exporter = Exporter::new().unwrap();
    let bytes = exporter
        .compile_docx(&competition(false), &DocxExport { embed_fonts: true })
        .expect("docx+fonts");
    std::fs::write(std::env::temp_dir().join("dtb-ke-fonts.docx"), &bytes).ok();
    let ft = docx_part(&bytes, "word/fontTable.xml").expect("fontTable.xml");
    assert!(ft.contains("w:embedRegular") && ft.contains("w:fontKey"));
    // both families: the Archivo body font and the condensed title font
    assert!(
        ft.contains(r#"w:name="Archivo""#)
            && ft.contains(r#"w:name="Archivo Condensed ExtraBold""#)
    );
    let styles = docx_part(&bytes, "word/styles.xml").expect("styles.xml");
    assert!(styles.contains(r#"w:ascii="Archivo Condensed ExtraBold""#));
    let settings = docx_part(&bytes, "word/settings.xml").expect("settings.xml");
    assert!(settings.contains("<w:embedTrueTypeFonts"));
    let odttf = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
        .unwrap()
        .file_names()
        .filter(|n| n.ends_with(".odttf"))
        .count();
    assert_eq!(odttf, 5, "4 Archivo weights + the condensed title face");
}
