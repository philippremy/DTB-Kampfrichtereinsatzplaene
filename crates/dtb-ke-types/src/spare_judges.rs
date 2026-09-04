use serde::{Deserialize, Serialize};

/// Reserve judges ("Ersatzkampfrichter") for a competition.
///
/// Split into qualification and finale to mirror [`crate::MeetingTimeDTO`] and
/// [`crate::JudgingTablesDTO`], which are already phase-split.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct SpareJudgesDTO {
    /// Reserve judges available during the qualification
    pub qualification: Vec<String>,
    /// Reserve judges available during the finale
    pub finale: Vec<String>,
}
