use serde::{Deserialize, Serialize};

/// Judging tables split per qualification and finale.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct JudgingTablesDTO {
    /// Tables for the qualification
    pub qualification: Vec<JudgingTableDTO>,
    /// Tables for the finale
    pub finale: Vec<JudgingTableDTO>,
}

/// A single judging table plus its user-defined label.
///
/// The label is free text set by the user (e.g. "Gerät 1", "Rhönrad Herren");
/// `kind` carries the discipline-specific judge assignments.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct JudgingTableDTO {
    /// User-defined name/label for this table
    pub label: String,
    /// The discipline-specific assignments
    pub kind: JudgingTableKindDTO,
}

/// A judging table for either Cyr or German Wheel.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum JudgingTableKindDTO {
    /// A Cyr Wheel table
    Cyr(CyrWheelTableDTO),
    /// A Gym Wheel table
    Gym(GymWheelTableDTO),
}

/// A Cyr Wheel table
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum CyrWheelTableDTO {
    /// A table for the Artistic Program
    Artistic(CyrWheelArtisticTableDTO),
    /// A table for the Technical Program
    Technical(CyrWheelTechnicalTableDTO),
}

/// A Gym Wheel table
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum GymWheelTableDTO {
    /// A table for Straight-Line
    STL(GymWheelSTLTableDTO),
    /// A table for Straight-Line with Music
    STLM(GymWheelSTLMTableDTO),
    /// A table for Spiral
    SPI(GymWheelSPITableDTO),
    /// A table for Vault
    VLT(GymWheelVLTTableDTO),
}

/// A table for the Artistic Program
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CyrWheelArtisticTableDTO {
    /// The head judge
    pub head: String,
    /// The first difficulty judge
    pub diff1: String,
    /// The second difficulty judge
    pub diff2: String,
    /// The first artistic-impression judge
    pub art1: String,
    /// The second artistic-impression judge
    pub art2: String,
    /// The third artistic-impression judge
    pub art3: String,
    /// The fourth artistic-impression judge
    pub art4: String,
}

/// A table for the Technical Program
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CyrWheelTechnicalTableDTO {
    /// The head judge
    pub head: String,
    /// The first difficulty judge
    pub diff1: String,
    /// The second difficulty judge
    pub diff2: String,
    /// The first execution judge
    pub exec1: String,
    /// The second execution judge
    pub exec2: String,
    /// The third execution judge
    pub exec3: String,
    /// The fourth execution judge
    pub exec4: String,
}

/// A table for Straight-Line
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct GymWheelSTLTableDTO {
    /// The head judge
    pub head: String,
    /// The first difficulty judge
    pub diff1: String,
    /// The second difficulty judge
    pub diff2: String,
    /// The first execution judge
    pub exec1: String,
    /// The second execution judge
    pub exec2: String,
    /// The third execution judge
    pub exec3: String,
    /// The fourth execution judge
    pub exec4: String,
}

/// A table for Straight-Line with Music
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct GymWheelSTLMTableDTO {
    /// The head judge
    pub head: String,
    /// The first difficulty judge
    pub diff1: String,
    /// The second difficulty judge
    pub diff2: String,
    /// The first execution judge
    pub exec1: String,
    /// The second execution judge
    pub exec2: String,
    /// The third execution judge
    pub exec3: String,
    /// The fourth execution judge
    pub exec4: String,
    /// The first artistic-impression judge
    pub art1: String,
    /// The second artistic-impression judge
    pub art2: String,
    /// The third artistic-impression judge
    pub art3: String,
    /// The fourth artistic-impression judge
    pub art4: String,
}

/// A table for Spiral
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct GymWheelSPITableDTO {
    /// The head judge
    pub head: String,
    /// The first difficulty judge
    pub diff1: String,
    /// The second difficulty judge
    pub diff2: String,
    /// The first execution judge
    pub exec1: String,
    /// The second execution judge
    pub exec2: String,
    /// The third execution judge
    pub exec3: String,
    /// The fourth execution judge
    pub exec4: String,
}

/// A table for Vault
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct GymWheelVLTTableDTO {
    /// The head judge
    pub head: String,
    /// The first difficulty judge
    pub diff1: String,
    /// The second difficulty judge
    pub diff2: String,
    /// The first execution judge
    pub exec1: String,
    /// The second execution judge
    pub exec2: String,
    /// The third execution judge
    pub exec3: String,
    /// The fourth execution judge
    pub exec4: String,
}
