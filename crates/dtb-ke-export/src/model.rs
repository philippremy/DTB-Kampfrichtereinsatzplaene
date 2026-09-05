//! The flat, pre-digested view of a [`CompetitionDTO`] that the Typst template
//! consumes as `json("data.json")`.
//!
//! Building this model is the *only* place competition data is transformed for
//! the document, and it is plain struct construction + `serde_json` — no
//! markup, no escaping, no regex. Typst receives every judge name, label and
//! remark as a data value and renders it as content, so user input can never
//! be interpreted as Typst syntax.

use dtb_ke_types::{CompetitionDTO, JudgingTableDTO, MeetingTimeDTO, RichTextDTO};
use serde::Serialize;

use crate::disciplines;

#[derive(Debug, Serialize)]
pub(crate) struct DocumentModel {
    /// The competition name — the big (possibly two-line) page-one title.
    pub title: String,
    /// `"09.05.2026"`.
    pub date: String,
    /// Trimmed location, or `""` when unset (template then drops "in …").
    pub location: String,
    /// Qualification tables, then (if any) finale tables. No visible phase
    /// headings — finale tables are marked by a `" (Finale)"` suffix on their
    /// discipline line instead.
    pub phases: Vec<Phase>,
    pub briefing: Briefing,
    /// Styled trailing free text; empty when the user left it blank.
    pub remarks: Vec<Paragraph>,
    /// `/assets/…` path of the header logo (org emblem, top-left), or `null`
    /// when no artwork is embedded — the template then draws a text monogram.
    pub org_logo: Option<String>,
    /// The emblem's on-page frame, in cm (aspect-preserved, from
    /// `crate::svg::emblem_frame_cm`). The template draws it at exactly this
    /// size so it is flush-left, not centred inside a fixed box. `null` when
    /// there is no emblem.
    pub org_logo_width_cm: Option<f64>,
    pub org_logo_height_cm: Option<f64>,
    /// `/assets/…` path of the swoosh (top-right), or `null`.
    pub turnen_logo: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Phase {
    /// The tables of one phase. Kept grouped (with no visible heading) so the
    /// finale tables always start a fresh table row rather than sharing one
    /// with a qualification table.
    pub tables: Vec<Table>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Table {
    /// The user-defined label — bold heading.
    pub label: String,
    /// Canonical discipline name — italic sub-heading. Finale tables carry a
    /// trailing `" (Finale)"`; qualification tables do not.
    pub discipline: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Row {
    pub role: String,
    pub judge: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Briefing {
    /// The time clause of the briefing sentence, assembled here so the template
    /// never branches on unified vs. split: `"um 09:00 Uhr"` or
    /// `"um 09:00 Uhr (Qualifikation) bzw. 13:00 Uhr (Finale)"`.
    pub time_clause: String,
    /// Reserve judges. One group (no label) when only one phase has any; two
    /// labelled groups otherwise; empty when there are none.
    pub spare: Vec<SpareGroup>,
    pub responsible: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SpareGroup {
    pub label: Option<String>,
    pub names: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Paragraph {
    pub runs: Vec<Run>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// `#RRGGBB`, or `None` for the document's default text color.
    pub color: Option<String>,
}

/// Build the template model from a competition.
pub(crate) fn build(
    comp: &CompetitionDTO,
    org_logo: Option<String>,
    org_logo_frame_cm: Option<(f64, f64)>,
    turnen_logo: Option<String>,
) -> DocumentModel {
    let quali = &comp.judging_tables.qualification;
    let finale = &comp.judging_tables.finale;

    let mut phases = Vec::new();
    if !quali.is_empty() {
        phases.push(Phase {
            tables: quali.iter().map(|t| table(t, false)).collect(),
        });
    }
    if !finale.is_empty() {
        phases.push(Phase {
            tables: finale.iter().map(|t| table(t, true)).collect(),
        });
    }

    DocumentModel {
        title: comp.name.trim().to_owned(),
        date: comp.date.format("%d.%m.%Y").to_string(),
        location: comp.location.trim().to_owned(),
        phases,
        briefing: briefing(comp),
        remarks: remarks(&comp.additional_remarks),
        org_logo,
        org_logo_width_cm: org_logo_frame_cm.map(|(w, _)| w),
        org_logo_height_cm: org_logo_frame_cm.map(|(_, h)| h),
        turnen_logo,
    }
}

fn table(dto: &JudgingTableDTO, finale: bool) -> Table {
    let mut discipline = disciplines::canonical_name(&dto.kind).to_owned();
    if finale {
        discipline.push_str(" (Finale)");
    }
    Table {
        label: dto.label.trim().to_owned(),
        discipline,
        rows: disciplines::role_rows(&dto.kind)
            .into_iter()
            .map(|(role, judge)| Row {
                role: role.to_owned(),
                judge: judge.trim().to_owned(),
            })
            .collect(),
    }
}

fn briefing(comp: &CompetitionDTO) -> Briefing {
    let time_clause = match &comp.meeting_times {
        MeetingTimeDTO::Unified(t) => format!("um {} Uhr", t.format("%H:%M")),
        MeetingTimeDTO::Split {
            qualification,
            finale,
        } => format!(
            "um {} Uhr (Qualifikation) bzw. {} Uhr (Finale)",
            qualification.format("%H:%M"),
            finale.format("%H:%M"),
        ),
    };

    let q = clean(&comp.spare_judges.qualification);
    let f = clean(&comp.spare_judges.finale);
    let spare = match (q.is_empty(), f.is_empty()) {
        (true, true) => Vec::new(),
        (false, true) => vec![SpareGroup {
            label: None,
            names: q,
        }],
        (true, false) => vec![SpareGroup {
            label: Some("Finale".to_owned()),
            names: f,
        }],
        (false, false) => vec![
            SpareGroup {
                label: Some("Qualifikation".to_owned()),
                names: q,
            },
            SpareGroup {
                label: Some("Finale".to_owned()),
                names: f,
            },
        ],
    };

    Briefing {
        time_clause,
        spare,
        responsible: clean(&comp.responsible_persons),
    }
}

fn remarks(rich: &RichTextDTO) -> Vec<Paragraph> {
    let paragraphs: Vec<Paragraph> = rich
        .paragraphs
        .iter()
        .map(|p| Paragraph {
            runs: p
                .runs
                .iter()
                .map(|r| Run {
                    text: r.text.clone(),
                    bold: r.bold,
                    italic: r.italic,
                    underline: r.underline,
                    color: r.color.clone(),
                })
                .collect(),
        })
        .collect();

    // Drop when every run of every paragraph is empty.
    if paragraphs
        .iter()
        .all(|p| p.runs.iter().all(|r| r.text.trim().is_empty()))
    {
        Vec::new()
    } else {
        paragraphs
    }
}

fn clean(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};
    use dtb_ke_types::*;
    use uuid::Uuid;

    fn sample() -> CompetitionDTO {
        CompetitionDTO {
            id: Uuid::nil(),
            name: "1. WM-Qualifikation 2026 (Erwachsene)".into(),
            organization: OrganizationDTO::DTB,
            location: "Brilon".into(),
            date: NaiveDate::from_ymd_opt(2026, 5, 9).unwrap(),
            meeting_times: MeetingTimeDTO::Unified(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            responsible_persons: vec!["Max Mustermann".into()],
            spare_judges: SpareJudgesDTO {
                qualification: vec!["Erika Musterfrau".into()],
                finale: vec![],
            },
            judging_tables: JudgingTablesDTO {
                qualification: vec![JudgingTableDTO {
                    label: "Gerade mit Musik".into(),
                    kind: JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(Default::default())),
                }],
                finale: vec![],
            },
            additional_remarks: RichTextDTO::default(),
        }
    }

    #[test]
    fn single_phase_json_round_trips() {
        let m = build(
            &sample(),
            Some("/assets/org.svg".into()),
            Some((6.0, 1.2)),
            Some("/assets/turnen.svg".into()),
        );
        assert_eq!(m.phases.len(), 1);
        assert_eq!(m.phases[0].tables[0].discipline, "Geradeturnen auf Musik");
        assert_eq!(m.phases[0].tables[0].rows.len(), 11);
        assert_eq!(m.briefing.time_clause, "um 09:00 Uhr");
        assert_eq!(m.briefing.spare.len(), 1);
        assert!(m.briefing.spare[0].label.is_none());
        // Must serialise.
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("Geradeturnen auf Musik"));
    }

    #[test]
    fn only_finale_tables_get_the_suffix() {
        let mut c = sample();
        c.judging_tables.finale = vec![JudgingTableDTO {
            label: "Sprung".into(),
            kind: JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(Default::default())),
        }];
        c.spare_judges.finale = vec!["A".into(), "B".into()];
        c.meeting_times = MeetingTimeDTO::Split {
            qualification: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            finale: NaiveTime::from_hms_opt(13, 30, 0).unwrap(),
        };
        let m = build(&c, None, None, None);
        assert_eq!(m.phases.len(), 2);
        assert_eq!(m.phases[0].tables[0].discipline, "Geradeturnen auf Musik");
        assert_eq!(
            m.phases[1].tables[0].discipline,
            "Geradeturnen auf Musik (Finale)"
        );
        assert!(m.briefing.time_clause.contains("bzw. 13:30 Uhr (Finale)"));
        assert_eq!(m.briefing.spare.len(), 2);
    }
}
