use chrono::{Datelike, NaiveDate};
use dtb_ke_types::{
    CompetitionDTO, JudgingTableDTO, JudgingTableKindDTO, JudgingTablesDTO, MeetingTimeDTO,
    OrganizationDTO, SpareJudgesDTO,
};
use uuid::Uuid;

use super::RichText;

/// The fully-decoded, editor-side representation of one competition.
///
/// Note the regrouping versus [`CompetitionDTO`]: the DTO keeps `judging_tables`
/// and `spare_judges` as independent phase-split structs, whereas here each
/// phase is a [`Round`] bundling its tables with its reserve judges — which is
/// how the UI presents them.
#[derive(Clone, Debug)]
pub struct Competition {
    pub id: Uuid,
    pub meta: CompetitionMeta,
    pub qualification: Round,
    pub finale: Round,
    pub remarks: RichText,
}

/// Everything about a competition that is not a judging table or reserve judge.
#[derive(Clone, Debug)]
pub struct CompetitionMeta {
    pub name: String,
    pub organization: OrganizationDTO,
    pub location: String,
    pub date: NaiveDate,
    pub meeting_times: MeetingTimeDTO,
    pub responsible_persons: Vec<String>,
}

/// One competition phase: its judging tables plus its reserve judges.
#[derive(Clone, Debug, Default)]
pub struct Round {
    pub tables: Vec<JudgingTable>,
    pub spare_judges: Vec<String>,
}

/// A judging table: a user-defined label plus the discipline-specific
/// assignments (reused verbatim from the DTO layer).
#[derive(Clone, Debug)]
pub struct JudgingTable {
    pub label: String,
    pub kind: JudgingTableKindDTO,
}

impl Competition {
    /// The calendar year, used as the sidebar sort key.
    pub fn year(&self) -> i32 {
        self.meta.date.year()
    }
}

impl From<CompetitionDTO> for Competition {
    fn from(dto: CompetitionDTO) -> Self {
        let CompetitionDTO {
            id,
            name,
            organization,
            location,
            date,
            meeting_times,
            responsible_persons,
            spare_judges,
            judging_tables,
            additional_remarks,
        } = dto;

        Self {
            id,
            meta: CompetitionMeta {
                name,
                organization,
                location,
                date,
                meeting_times,
                responsible_persons,
            },
            qualification: Round {
                tables: judging_tables
                    .qualification
                    .into_iter()
                    .map(JudgingTable::from)
                    .collect(),
                spare_judges: spare_judges.qualification,
            },
            finale: Round {
                tables: judging_tables
                    .finale
                    .into_iter()
                    .map(JudgingTable::from)
                    .collect(),
                spare_judges: spare_judges.finale,
            },
            remarks: additional_remarks.into(),
        }
    }
}

impl From<&Competition> for CompetitionDTO {
    fn from(model: &Competition) -> Self {
        Self {
            id: model.id,
            name: model.meta.name.clone(),
            organization: model.meta.organization,
            location: model.meta.location.clone(),
            date: model.meta.date,
            meeting_times: model.meta.meeting_times.clone(),
            responsible_persons: model.meta.responsible_persons.clone(),
            spare_judges: SpareJudgesDTO {
                qualification: model.qualification.spare_judges.clone(),
                finale: model.finale.spare_judges.clone(),
            },
            judging_tables: JudgingTablesDTO {
                qualification: model
                    .qualification
                    .tables
                    .iter()
                    .map(JudgingTableDTO::from)
                    .collect(),
                finale: model
                    .finale
                    .tables
                    .iter()
                    .map(JudgingTableDTO::from)
                    .collect(),
            },
            additional_remarks: (&model.remarks).into(),
        }
    }
}

impl From<JudgingTableDTO> for JudgingTable {
    fn from(dto: JudgingTableDTO) -> Self {
        Self {
            label: dto.label,
            kind: dto.kind,
        }
    }
}

impl From<&JudgingTable> for JudgingTableDTO {
    fn from(model: &JudgingTable) -> Self {
        Self {
            label: model.label.clone(),
            kind: model.kind.clone(),
        }
    }
}
