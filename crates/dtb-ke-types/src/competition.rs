use chrono::{Datelike, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{JudgingTablesDTO, OrganizationDTO, RichTextDTO, SpareJudgesDTO};

/// The meeting time for the judges
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum MeetingTimeDTO {
    /// A single meeting time for the whole competition
    Unified(NaiveTime),
    /// Split meeting times for the qualifications and the finale
    Split {
        qualification: NaiveTime,
        finale: NaiveTime,
    },
}

/// A single, unique competition.
///
/// This is the aggregate root of the persisted data model: exactly one
/// `CompetitionDTO` is postcard-encoded into the `data` blob of one row in the
/// `competitions` table (see `dtb-ke-persist`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CompetitionDTO {
    /// The unique ID of the competition (also the database primary key)
    pub id: Uuid,
    /// The name of the competition
    pub name: String,
    /// The responsible organization
    pub organization: OrganizationDTO,
    /// The location of the competition
    pub location: String,
    /// The date of the competition
    pub date: NaiveDate,
    /// The meeting time(s) for the judges
    pub meeting_times: MeetingTimeDTO,
    /// The responsible person(s)
    pub responsible_persons: Vec<String>,
    /// Reserve judges ("Ersatzkampfrichter"), split by phase
    pub spare_judges: SpareJudgesDTO,
    /// The judging table(s)
    pub judging_tables: JudgingTablesDTO,
    /// Free-form styled text appended to the end of the generated document
    pub additional_remarks: RichTextDTO,
}

impl CompetitionDTO {
    /// The calendar year of the competition. Stored as its own database column
    /// so the sidebar list can be built and sorted without decoding blobs.
    pub fn year(&self) -> i32 {
        self.date.year()
    }
}
