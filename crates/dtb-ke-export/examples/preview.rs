//! Render a representative competition to `<out>/plan.pdf` plus one PNG per
//! page. Run with:
//!
//! ```sh
//! cargo run -p dtb-ke-export --example preview -- /tmp/out
//! ```

use std::fs;
use std::path::PathBuf;

use chrono::{NaiveDate, NaiveTime};
use dtb_ke_export::{Exporter, PdfExport, PreviewOptions};
use dtb_ke_types::*;
use uuid::Uuid;

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    fs::create_dir_all(&out).unwrap();

    let exporter = Exporter::new().expect("exporter");

    for (name, competition) in [("plan", sample()), ("stress", stress())] {
        let pdf = exporter
            .compile_pdf(&competition, &PdfExport::default())
            .expect("pdf");
        fs::write(out.join(format!("{name}.pdf")), &pdf).unwrap();
        println!("wrote {name}.pdf ({} bytes)", pdf.len());

        let pages = exporter
            .render_previews(
                &competition,
                PreviewOptions {
                    pixels_per_point: 2.0,
                },
            )
            .expect("previews");
        for (i, page) in pages.iter().enumerate() {
            let path = out.join(format!("{name}-page-{}.png", i + 1));
            write_png(&path, page.width, page.height, &page.rgba);
            println!("  {} ({}x{})", path.display(), page.width, page.height);
        }
    }
}

/// Worst case for page one: a title that wraps to two lines, and two full
/// STL-M pairs that must still fit under it.
fn stress() -> CompetitionDTO {
    let mut c = sample();
    c.name = "24. Deutsche Vereinsmannschaftsmeisterschaften (Senioren)".into();
    c.judging_tables.qualification = vec![
        stlm("Gerade m. M. 1"),
        stlm("Gerade m. M. 2"),
        stlm("Gerade m. M. 3"),
        spi("Spirale"),
    ];
    c.judging_tables.finale = vec![stlm("Finale Gerade m. Musik"), vlt("Finale Sprung")];
    c.spare_judges.finale = vec!["Otto Ersatz".into()];
    c.meeting_times = MeetingTimeDTO::Split {
        qualification: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        finale: NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
    };
    c
}

fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    let file = fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

fn stlm(label: &str) -> JudgingTableDTO {
    JudgingTableDTO {
        label: label.into(),
        kind: JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(GymWheelSTLMTableDTO {
            head: "Anna Beispiel".into(),
            diff1: "Ben Muster".into(),
            diff2: "Carla Test".into(),
            exec1: "Dana Probe".into(),
            exec2: "Emil Übung".into(),
            exec3: "Fine Vorlage".into(),
            exec4: "Gero Muster".into(),
            art1: "Hanna Beispiel".into(),
            art2: "Ivo Test".into(),
            art3: "Jana Probe".into(),
            art4: "Klaus Muster".into(),
        })),
    }
}

fn spi(label: &str) -> JudgingTableDTO {
    JudgingTableDTO {
        label: label.into(),
        kind: JudgingTableKindDTO::Gym(GymWheelTableDTO::SPI(GymWheelSPITableDTO {
            head: "Lena Beispiel".into(),
            diff1: "Mia Muster".into(),
            diff2: "Nils Test".into(),
            exec1: "Ole Probe".into(),
            exec2: "Pia Übung".into(),
            exec3: "Rene Vorlage".into(),
            exec4: "Sina Muster".into(),
        })),
    }
}

fn vlt(label: &str) -> JudgingTableDTO {
    JudgingTableDTO {
        label: label.into(),
        kind: JudgingTableKindDTO::Gym(GymWheelTableDTO::VLT(GymWheelVLTTableDTO {
            head: "Tom Beispiel".into(),
            diff1: "Uwe Muster".into(),
            diff2: "Vera Test".into(),
            exec1: "Wim Probe".into(),
            exec2: "Xenia Übung".into(),
            exec3: "Yara Vorlage".into(),
            exec4: "Zoe Muster".into(),
        })),
    }
}

fn sample() -> CompetitionDTO {
    CompetitionDTO {
        id: Uuid::new_v4(),
        name: "1. WM-Qualifikation 2026 (Erwachsene)".into(),
        organization: OrganizationDTO::DTB,
        location: "Brilon".into(),
        date: NaiveDate::from_ymd_opt(2026, 5, 9).unwrap(),
        meeting_times: MeetingTimeDTO::Unified(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
        responsible_persons: vec!["Max Mustermann".into(), "Erika Musterfrau".into()],
        spare_judges: SpareJudgesDTO {
            qualification: vec!["Nina Reserve".into(), "Paul Ersatz".into()],
            finale: vec![],
        },
        judging_tables: JudgingTablesDTO {
            qualification: vec![stlm("Gerade mit Musik"), spi("Spirale"), vlt("Sprung")],
            finale: vec![],
        },
        additional_remarks: RichTextDTO {
            paragraphs: vec![RichParagraphDTO {
                runs: vec![
                    RichRunDTO {
                        text: "Hinweis: ".into(),
                        bold: true,
                        ..Default::default()
                    },
                    RichRunDTO {
                        text: "Bitte pünktlich erscheinen.".into(),
                        italic: true,
                        ..Default::default()
                    },
                ],
            }],
        },
    }
}
