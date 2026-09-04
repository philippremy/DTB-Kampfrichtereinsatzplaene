use std::path::Path;

use chrono::{Datelike, NaiveDate};
use dtb_ke_types::CompetitionDTO;
use log::{debug, info, trace, warn};
use turso::{Builder, Connection, Value};
use uuid::Uuid;

use crate::{PersistError, PersistResult, competition_dto_from_bytes, competition_dto_to_bytes};

/// How dates are stored in the `date` column — ISO `YYYY-MM-DD` sorts
/// chronologically as plain text.
const DATE_SQL_FMT: &str = "%Y-%m-%d";

/// The current on-disk schema version.
///
/// Bump this on any breaking change to [`CompetitionDTO`]. There is no migration
/// path yet (the app is pre-release), so bumping simply invalidates existing
/// `PersistedSessions.bin` files — [`load_competition`] returns
/// [`PersistError::SchemaVersion`] for rows written by an older build.
pub const SCHEMA_VERSION: u32 = 1;

const INIT_SQL: &str = "\
CREATE TABLE IF NOT EXISTS competitions (
    id             BLOB    PRIMARY KEY,
    date           TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    schema_version INTEGER NOT NULL,
    data           BLOB    NOT NULL
);
CREATE INDEX IF NOT EXISTS competitions_date ON competitions (date);
";

/// A blob-free projection of a competition row, used to populate the navigation
/// sidebar without decoding every stored competition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompetitionSummary {
    pub id: Uuid,
    pub date: NaiveDate,
    pub name: String,
}

impl CompetitionSummary {
    /// The calendar year — the sidebar's grouping key.
    pub fn year(&self) -> i32 {
        self.date.year()
    }
}

/// Owns the turso database handle and a connection to it.
///
/// This is intentionally a plain owned value rather than a global singleton
/// (unlike `FilesystemHelper` in the UI crate): opening is async and the handle
/// is kept inside the UI's root store entity. `turso::Database` stays on the
/// thread that opened it; clone [`Db::connection`] to run queries elsewhere
/// (e.g. on a background executor).
pub struct Db {
    _database: turso::Database,
    connection: Connection,
}

impl Db {
    /// Opens (creating if necessary) the database at `path` and ensures the
    /// schema exists.
    pub async fn open(path: &Path) -> PersistResult<Self> {
        let path = path.to_string_lossy();
        debug!("opening turso database at {path}");
        let database = Builder::new_local(&path).build().await?;
        let connection = database.connect()?;
        connection.execute_batch(INIT_SQL).await?;
        // Pre-release: a database written by a build that still had the old
        // `year INTEGER` column is rebuilt from scratch rather than migrated.
        if is_legacy_schema(&connection).await? {
            warn!(
                "legacy schema (pre-`date` column) detected — rebuilding `competitions` from scratch"
            );
            connection.execute_batch("DROP TABLE competitions;").await?;
            connection.execute_batch(INIT_SQL).await?;
        }
        info!("database open, schema v{SCHEMA_VERSION}");
        Ok(Self {
            _database: database,
            connection,
        })
    }

    /// A cheap clone of the connection, safe to move onto another thread or
    /// executor. All clones share the same underlying database.
    pub fn connection(&self) -> Connection {
        self.connection.clone()
    }
}

/// Whether `competitions` exists with the pre-`date` column layout.
async fn is_legacy_schema(conn: &Connection) -> PersistResult<bool> {
    let mut rows = conn.query("PRAGMA table_info(competitions)", ()).await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        if let Value::Text(name) = row.get_value(1)? {
            columns.push(name);
        }
    }
    // A brand-new DB has just been created with the current schema (has `date`);
    // only a leftover old table lacks it while still having rows/columns.
    Ok(!columns.is_empty() && !columns.iter().any(|c| c == "date"))
}

/// Lists every stored competition, newest date first, then by name.
pub async fn list_summaries(conn: &Connection) -> PersistResult<Vec<CompetitionSummary>> {
    let mut rows = conn
        .query(
            "SELECT id, date, name FROM competitions ORDER BY date DESC, name ASC",
            (),
        )
        .await?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let id = blob_uuid(row.get_value(0)?, "id")?;
        let date = match row.get_value(1)? {
            Value::Text(s) => NaiveDate::parse_from_str(&s, DATE_SQL_FMT)
                .map_err(|e| PersistError::RowShape(format!("date {s:?}: {e}")))?,
            other => return Err(PersistError::RowShape(format!("date: {other:?}"))),
        };
        let name = match row.get_value(2)? {
            Value::Text(n) => n,
            other => return Err(PersistError::RowShape(format!("name: {other:?}"))),
        };
        out.push(CompetitionSummary { id, date, name });
    }
    trace!("list_summaries → {} row(s)", out.len());
    Ok(out)
}

/// Loads and decodes a single competition, or `None` if the id is unknown.
pub async fn load_competition(
    conn: &Connection,
    id: Uuid,
) -> PersistResult<Option<CompetitionDTO>> {
    let mut rows = conn
        .query(
            "SELECT schema_version, data FROM competitions WHERE id = ?1",
            (id.as_bytes().to_vec(),),
        )
        .await?;

    let Some(row) = rows.next().await? else {
        trace!("load_competition({id}) → no such row");
        return Ok(None);
    };

    let found = match row.get_value(0)? {
        Value::Integer(v) => v as u32,
        other => return Err(PersistError::RowShape(format!("schema_version: {other:?}"))),
    };
    if found != SCHEMA_VERSION {
        warn!(
            "load_competition({id}): schema v{found} on disk, this build expects v{SCHEMA_VERSION}"
        );
        return Err(PersistError::SchemaVersion {
            id,
            found,
            expected: SCHEMA_VERSION,
        });
    }

    let data = match row.get_value(1)? {
        Value::Blob(bytes) => bytes,
        other => return Err(PersistError::RowShape(format!("data: {other:?}"))),
    };
    trace!("load_competition({id}) → {} bytes", data.len());
    Ok(Some(competition_dto_from_bytes(&data)?))
}

/// Inserts or replaces a competition. `id`, `date` and `name` are stored as
/// their own columns; the whole DTO is postcard-encoded into the `data` blob.
pub async fn save_competition(
    conn: &Connection,
    competition: &CompetitionDTO,
) -> PersistResult<()> {
    let data = competition_dto_to_bytes(competition)?;
    trace!(
        "save_competition({}) — {} bytes",
        competition.id,
        data.len()
    );
    conn.execute(
        "INSERT OR REPLACE INTO competitions (id, date, name, schema_version, data)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        (
            competition.id.as_bytes().to_vec(),
            competition.date.format(DATE_SQL_FMT).to_string(),
            competition.name.clone(),
            SCHEMA_VERSION as i64,
            data,
        ),
    )
    .await?;
    Ok(())
}

/// Permanently removes a competition. A no-op if the id is unknown.
pub async fn delete_competition(conn: &Connection, id: Uuid) -> PersistResult<()> {
    let n = conn
        .execute(
            "DELETE FROM competitions WHERE id = ?1",
            (id.as_bytes().to_vec(),),
        )
        .await?;
    trace!("delete_competition({id}) — {n} row(s) removed");
    Ok(())
}

fn blob_uuid(value: Value, column: &str) -> PersistResult<Uuid> {
    match value {
        Value::Blob(bytes) => Ok(Uuid::from_slice(&bytes)?),
        other => Err(PersistError::RowShape(format!("{column}: {other:?}"))),
    }
}
