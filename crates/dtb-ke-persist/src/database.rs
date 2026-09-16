use std::path::Path;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dtb_ke_types::CompetitionDTO;
use log::{debug, info, trace, warn};
use turso::{Builder, Connection, Value};
use uuid::Uuid;

use crate::{PersistError, PersistResult, competition_dto_from_bytes, competition_dto_to_bytes};

/// How dates are stored in the `date` column — ISO `YYYY-MM-DD` sorts
/// chronologically as plain text.
const DATE_SQL_FMT: &str = "%Y-%m-%d";

/// How `deleted_at` is stored — ISO-8601 UTC, e.g. `2026-09-16T14:03:00Z`.
const DELETED_AT_SQL_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// The current on-disk schema version.
///
/// Bump this on any breaking change to [`CompetitionDTO`]. There is no migration
/// path yet (the app is pre-release), so bumping simply invalidates existing
/// `PersistedSessions.bin` files — [`load_competition`] returns
/// [`PersistError::SchemaVersion`] for rows written by an older build.
pub const SCHEMA_VERSION: u32 = 1;

// Table creation and index creation are deliberately separate statements: an
// upgrade from an older on-disk schema still has to run `CREATE TABLE IF NOT
// EXISTS` against the *existing* table (a no-op) before the migration check
// below can tell whether it needs rebuilding — and a `CREATE INDEX` on
// `deleted_at` at that point would fail outright, since the pre-migration
// table doesn't have that column yet.
const CREATE_TABLE_SQL: &str = "\
CREATE TABLE IF NOT EXISTS competitions (
    id             BLOB    PRIMARY KEY,
    date           TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    schema_version INTEGER NOT NULL,
    data           BLOB    NOT NULL,
    deleted_at     TEXT
);
";
const CREATE_INDEXES_SQL: &str = "\
CREATE INDEX IF NOT EXISTS competitions_date ON competitions (date);
CREATE INDEX IF NOT EXISTS competitions_deleted_at ON competitions (deleted_at);
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

/// A blob-free projection of a *trashed* (soft-deleted) competition row — the
/// trash window's own listing, parallel to [`CompetitionSummary`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrashedCompetition {
    pub id: Uuid,
    pub date: NaiveDate,
    pub name: String,
    pub deleted_at: DateTime<Utc>,
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
        connection.execute_batch(CREATE_TABLE_SQL).await?;
        // Pre-release: a database written by a build that still had the old
        // `year INTEGER` column, or is missing the newer `deleted_at` column
        // (the competition trash), is rebuilt from scratch rather than
        // migrated — there's no real user data to protect yet.
        if is_legacy_schema(&connection).await? || is_missing_deleted_at(&connection).await? {
            warn!(
                "outdated `competitions` schema detected — rebuilding it from scratch"
            );
            connection.execute_batch("DROP TABLE competitions;").await?;
            connection.execute_batch(CREATE_TABLE_SQL).await?;
        }
        connection.execute_batch(CREATE_INDEXES_SQL).await?;
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
    let columns = table_columns(conn).await?;
    // A brand-new DB has just been created with the current schema (has `date`);
    // only a leftover old table lacks it while still having rows/columns.
    Ok(!columns.is_empty() && !columns.iter().any(|c| c == "date"))
}

/// Whether `competitions` exists but predates the `deleted_at` column (the
/// competition trash).
async fn is_missing_deleted_at(conn: &Connection) -> PersistResult<bool> {
    let columns = table_columns(conn).await?;
    Ok(!columns.is_empty() && !columns.iter().any(|c| c == "deleted_at"))
}

async fn table_columns(conn: &Connection) -> PersistResult<Vec<String>> {
    let mut rows = conn.query("PRAGMA table_info(competitions)", ()).await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        if let Value::Text(name) = row.get_value(1)? {
            columns.push(name);
        }
    }
    Ok(columns)
}

/// Lists every non-trashed competition, newest date first, then by name.
pub async fn list_summaries(conn: &Connection) -> PersistResult<Vec<CompetitionSummary>> {
    let mut rows = conn
        .query(
            "SELECT id, date, name FROM competitions WHERE deleted_at IS NULL \
             ORDER BY date DESC, name ASC",
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

/// Lists every trashed (soft-deleted) competition, most recently deleted first.
pub async fn list_trashed(conn: &Connection) -> PersistResult<Vec<TrashedCompetition>> {
    let mut rows = conn
        .query(
            "SELECT id, date, name, deleted_at FROM competitions \
             WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC",
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
        let deleted_at = match row.get_value(3)? {
            // `DateTime::parse_from_str` needs a real offset *directive*
            // (`%z`/`%#z`) in the format string to parse into a `FixedOffset`
            // — a literal `Z` doesn't count as one. The stored string is UTC
            // by construction (we always format it with `DELETED_AT_SQL_FMT`
            // ourselves), so parse it as naive and attach UTC directly.
            Value::Text(s) => chrono::NaiveDateTime::parse_from_str(&s, DELETED_AT_SQL_FMT)
                .map_err(|e| PersistError::RowShape(format!("deleted_at {s:?}: {e}")))?
                .and_utc(),
            other => return Err(PersistError::RowShape(format!("deleted_at: {other:?}"))),
        };
        out.push(TrashedCompetition {
            id,
            date,
            name,
            deleted_at,
        });
    }
    trace!("list_trashed → {} row(s)", out.len());
    Ok(out)
}

/// Moves a competition to the trash (sets `deleted_at`). A no-op if the id is
/// unknown or already trashed.
pub async fn soft_delete_competition(
    conn: &Connection,
    id: Uuid,
    deleted_at: DateTime<Utc>,
) -> PersistResult<()> {
    let n = conn
        .execute(
            "UPDATE competitions SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            (
                deleted_at.format(DELETED_AT_SQL_FMT).to_string(),
                id.as_bytes().to_vec(),
            ),
        )
        .await?;
    trace!("soft_delete_competition({id}) — {n} row(s) trashed");
    Ok(())
}

/// Moves every competition in `ids` to the trash, in a single statement.
/// Unknown/already-trashed ids are silently skipped; a no-op for an empty
/// slice. One `UPDATE … WHERE id IN (…)` rather than one `UPDATE` per id —
/// many concurrent single-row statements against clones of the same
/// `Connection` (as a naive per-id loop would issue) hit turso's "concurrent
/// use forbidden" guard once enough are in flight at once.
pub async fn soft_delete_competitions(
    conn: &Connection,
    ids: &[Uuid],
    deleted_at: DateTime<Utc>,
) -> PersistResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = (2..=ids.len() + 1)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE competitions SET deleted_at = ?1 WHERE id IN ({placeholders}) \
         AND deleted_at IS NULL"
    );
    let mut params: Vec<Value> = vec![Value::Text(deleted_at.format(DELETED_AT_SQL_FMT).to_string())];
    params.extend(ids.iter().map(|id| Value::Blob(id.as_bytes().to_vec())));
    let n = conn
        .execute(sql, turso::params_from_iter(params))
        .await?;
    trace!(
        "soft_delete_competitions({} ids) — {n} row(s) trashed",
        ids.len()
    );
    Ok(())
}

/// Restores a trashed competition (clears `deleted_at`). A no-op if the id is
/// unknown or not trashed.
pub async fn restore_competition(conn: &Connection, id: Uuid) -> PersistResult<()> {
    let n = conn
        .execute(
            "UPDATE competitions SET deleted_at = NULL WHERE id = ?1",
            (id.as_bytes().to_vec(),),
        )
        .await?;
    trace!("restore_competition({id}) — {n} row(s) restored");
    Ok(())
}

/// Permanently removes one trashed competition. A no-op if the id is unknown
/// or — deliberately — not currently trashed, so this can never remove a row
/// that hasn't gone through the trash first.
pub async fn purge_competition(conn: &Connection, id: Uuid) -> PersistResult<()> {
    let n = conn
        .execute(
            "DELETE FROM competitions WHERE id = ?1 AND deleted_at IS NOT NULL",
            (id.as_bytes().to_vec(),),
        )
        .await?;
    trace!("purge_competition({id}) — {n} row(s) removed");
    Ok(())
}

/// Empties the trash: permanently removes every trashed competition. Returns
/// the number of rows removed.
pub async fn purge_all_trashed(conn: &Connection) -> PersistResult<usize> {
    let n = conn
        .execute("DELETE FROM competitions WHERE deleted_at IS NOT NULL", ())
        .await?;
    trace!("purge_all_trashed — {n} row(s) removed");
    Ok(n as usize)
}

fn blob_uuid(value: Value, column: &str) -> PersistResult<Uuid> {
    match value {
        Value::Blob(bytes) => Ok(Uuid::from_slice(&bytes)?),
        other => Err(PersistError::RowShape(format!("{column}: {other:?}"))),
    }
}
