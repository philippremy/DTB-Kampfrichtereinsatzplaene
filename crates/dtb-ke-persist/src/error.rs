use thiserror::Error;
use uuid::Uuid;

pub type PersistResult<T> = Result<T, PersistError>;

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("deserialization failed: {0}")]
    Deserialization(#[source] postcard::Error),
    #[error("serialization failed: {0}")]
    Serialization(#[source] postcard::Error),
    #[error("database error: {0}")]
    Database(#[from] turso::Error),
    #[error("invalid competition id stored in database: {0}")]
    InvalidId(#[from] uuid::Error),
    #[error("unexpected column value: {0}")]
    RowShape(String),
    #[error(
        "stored competition {id} has schema version {found}, but this build expects {expected} \
         (no migration path yet — delete PersistedSessions.bin)"
    )]
    SchemaVersion { id: Uuid, found: u32, expected: u32 },
}
