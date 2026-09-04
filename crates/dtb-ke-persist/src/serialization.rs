use dtb_ke_types::CompetitionDTO;
use postcard::{from_bytes, to_stdvec};

use crate::{PersistError, PersistResult};

/// Attempts to deserialize a [CompetitionDTO] from raw postcard bytes.
pub fn competition_dto_from_bytes(bytes: &[u8]) -> PersistResult<CompetitionDTO> {
    from_bytes(bytes).map_err(PersistError::Deserialization)
}

/// Attempts to serialize a [CompetitionDTO] to raw postcard bytes.
pub fn competition_dto_to_bytes(competition_dto: &CompetitionDTO) -> PersistResult<Vec<u8>> {
    to_stdvec(competition_dto).map_err(PersistError::Serialization)
}
