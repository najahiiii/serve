use crate::utils::{parent_relative_path, relative_path_string};
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinError;
use tokio::time;
use tokio_rusqlite::Connection;
use ulid::Ulid;
use walkdir::WalkDir;

#[derive(Debug)]
pub enum CatalogError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Connection(tokio_rusqlite::Error),
    Join(JoinError),
}

impl From<std::io::Error> for CatalogError {
    fn from(err: std::io::Error) -> Self {
        CatalogError::Io(err)
    }
}

impl From<rusqlite::Error> for CatalogError {
    fn from(err: rusqlite::Error) -> Self {
        CatalogError::Sqlite(err)
    }
}

impl From<tokio_rusqlite::Error> for CatalogError {
    fn from(err: tokio_rusqlite::Error) -> Self {
        CatalogError::Connection(err)
    }
}

impl From<JoinError> for CatalogError {
    fn from(err: JoinError) -> Self {
        CatalogError::Join(err)
    }
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::Io(err) => write!(f, "IO error: {err}"),
            CatalogError::Sqlite(err) => write!(f, "SQLite error: {err}"),
            CatalogError::Connection(err) => write!(f, "Connection error: {err}"),
            CatalogError::Join(err) => write!(f, "Join error: {err}"),
        }
    }
}

impl std::error::Error for CatalogError {}

#[derive(Clone)]
pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    pub async fn new(path: &Path) -> Result<Self, CatalogError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).await?;
        conn.call(|conn| {
            conn.execute_batch(
                "
                PRAGMA journal_mode = WAL;
                CREATE TABLE IF NOT EXISTS entries (
                    id TEXT PRIMARY KEY,
                    path TEXT NOT NULL UNIQUE,
                    name TEXT NOT NULL,
                    parent_id TEXT,
                    is_dir INTEGER NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    mime_type TEXT,
                    modified INTEGER NOT NULL,
                    last_seen INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_entries_parent ON entries(parent_id);
                CREATE INDEX IF NOT EXISTS idx_entries_path ON entries(path);
                ",
            )?;
            Ok(())
        })
        .await?;

        Ok(Self { conn })
    }

    pub async fn sync_entry(&self, info: EntryInfo) -> Result<String, CatalogError> {
        let EntryInfo {
            relative_path,
            name,
            parent_path,
            is_dir,
            size_bytes,
            mime_type,
            modified,
        } = info;

        let params_relative = relative_path.clone();
        let params_parent = parent_path.clone();

        self.conn
            .call(move |conn| {
                let existing_id: Option<String> = conn
                    .query_row(
                        "SELECT id FROM entries WHERE path = ?1",
                        [params_relative.as_str()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;

                let id = existing_id.unwrap_or_else(|| Ulid::new().to_string());
                let parent_id = match params_parent {
                   Some(parent) => conn
                        .query_row(
                            "SELECT id FROM entries WHERE path = ?1",
                            [parent.as_str()],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?,
                    None => None,
                };

                let size = size_bytes.min(i64::MAX as u64) as i64;
                let now = current_unix_timestamp();

                conn.execute(
                    "INSERT INTO entries (id, path, name, parent_id, is_dir, size_bytes, mime_type, modified, last_seen)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(path) DO UPDATE SET
                        name=excluded.name,
                        parent_id=excluded.parent_id,
                        is_dir=excluded.is_dir,
                        size_bytes=excluded.size_bytes,
                        mime_type=excluded.mime_type,
                        modified=excluded.modified,
                        last_seen=excluded.last_seen",
                    params![
                        id,
                        params_relative,
                        name,
                        parent_id,
                        if is_dir { 1 } else { 0 },
                        size,
                        mime_type,
                        modified,
                        now
                    ],
                )?;

                Ok(id)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn refresh_full(
        &self,
        root: &Path,
        blacklist: &HashSet<String>,
    ) -> Result<(), CatalogError> {
        let root = root.to_path_buf();
        let blacklist = blacklist.clone();
        let entries = tokio::task::spawn_blocking(move || scan_root(&root, &blacklist))
            .await
            .map_err(CatalogError::from)??;
        self.apply_snapshot(entries).await
    }

    pub async fn resolve_id(&self, id: &str) -> Result<Option<CatalogEntry>, CatalogError> {
        let id = id.to_string();
        self.conn
            .call(move |conn| {
                let mut stmt = conn.prepare("SELECT path, is_dir FROM entries WHERE id = ?1")?;
                let mut rows = stmt.query([id.as_str()])?;
                if let Some(row) = rows.next()? {
                    let path: String = row.get(0)?;
                    let is_dir: i64 = row.get(1)?;
                    Ok(Some(CatalogEntry {
                        relative_path: path,
                        is_dir: is_dir != 0,
                    }))
                } else {
                    Ok(None)
                }
            })
            .await
            .map_err(Into::into)
    }

    async fn apply_snapshot(&self, entries: Vec<ScannedEntry>) -> Result<(), CatalogError> {
        let now = current_unix_timestamp();
        self.conn
            .call(move |conn| {
                let mut id_map = existing_ids(conn)?;
                let mut sorted = entries;
                sorted.sort_by_key(|entry| entry.depth);

                let tx = conn.transaction()?;
                let mut stmt = tx.prepare(
                    "INSERT INTO entries (id, path, name, parent_id, is_dir, size_bytes, mime_type, modified, last_seen)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(path) DO UPDATE SET
                        name=excluded.name,
                        parent_id=excluded.parent_id,
                        is_dir=excluded.is_dir,
                        size_bytes=excluded.size_bytes,
                        mime_type=excluded.mime_type,
                        modified=excluded.modified,
                        last_seen=excluded.last_seen",
                )?;

                for entry in sorted {
                    let path = entry.relative_path.clone();
                    let id = id_map
                        .entry(path.clone())
                        .or_insert_with(|| Ulid::new().to_string())
                        .clone();

                    let parent_id = entry
                        .parent_path
                        .as_ref()
                        .and_then(|parent| id_map.get(parent))
                        .cloned();

                    let size = entry.size_bytes.min(i64::MAX as u64) as i64;

                    stmt.execute(params![
                        id,
                        path,
                        entry.name,
                        parent_id,
                        if entry.is_dir { 1 } else { 0 },
                        size,
                        entry.mime_type,
                        entry.modified,
                        now
                    ])?;
                }

                drop(stmt);

                tx.execute(
                    "DELETE FROM entries WHERE last_seen <> ?1",
                    [now],
                )?;

                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(Into::into)
    }
}

fn existing_ids(conn: &rusqlite::Connection) -> Result<HashMap<String, String>, rusqlite::Error> {
    let mut stmt = conn.prepare("SELECT path, id FROM entries")?;
    let mut rows = stmt.query([])?;
    let mut map = HashMap::new();
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let id: String = row.get(1)?;
        map.insert(path, id);
    }
    Ok(map)
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64
}

struct ScannedEntry {
    relative_path: String,
    name: String,
    parent_path: Option<String>,
    is_dir: bool,
    size_bytes: u64,
    mime_type: String,
    modified: i64,
    depth: usize,
}

fn scan_root(
    root: &Path,
    blacklist: &HashSet<String>,
) -> Result<Vec<ScannedEntry>, std::io::Error> {
    let mut entries = Vec::new();

    let mut iter = WalkDir::new(root).into_iter();
    while let Some(entry) = iter.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("Metadata scan error: {}", err);
                continue;
            }
        };

        let full_path = entry.path();
        if crate::utils::is_blacklisted(full_path, root, blacklist) {
            if entry.file_type().is_dir() {
                iter.skip_current_dir();
            }
            continue;
        }

        let relative = match relative_path_string(root, full_path) {
            Some(path) => path,
            None => continue,
        };

        let metadata = match entry.metadata() {
            Ok(meta) => meta,
            Err(err) => {
                tracing::warn!(
                    "Failed to read metadata for {}: {}",
                    full_path.display(),
                    err
                );
                continue;
            }
        };

        let name = full_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| relative.clone());

        let parent_path = parent_relative_path(&relative);
        let is_dir = metadata.is_dir();
        let size_bytes = if is_dir { 0 } else { metadata.len() };
        let mime_type = if is_dir {
            "inode/directory".to_string()
        } else {
            mime_guess::MimeGuess::from_path(full_path)
                .first_raw()
                .unwrap_or("application/octet-stream")
                .to_string()
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let depth = relative
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count();

        entries.push(ScannedEntry {
            relative_path: relative,
            name,
            parent_path,
            is_dir,
            size_bytes,
            mime_type,
            modified,
            depth,
        });
    }

    Ok(entries)
}

#[derive(Clone)]
pub struct CatalogEntry {
    pub relative_path: String,
    pub is_dir: bool,
}

#[derive(Clone)]
pub struct EntryInfo {
    pub relative_path: String,
    pub name: String,
    pub parent_path: Option<String>,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub mime_type: String,
    pub modified: i64,
}

impl EntryInfo {
    pub fn new(
        relative_path: String,
        name: String,
        parent_path: Option<String>,
        is_dir: bool,
        size_bytes: u64,
        mime_type: String,
        modified: i64,
    ) -> Self {
        Self {
            relative_path,
            name,
            parent_path,
            is_dir,
            size_bytes,
            mime_type,
            modified,
        }
    }
}

#[derive(Debug)]
pub enum CatalogCommand {
    RefreshAll,
}

pub struct CatalogWorker {
    catalog: Arc<Catalog>,
    root: Arc<PathBuf>,
    blacklist: Arc<HashSet<String>>,
    interval: Duration,
    rx: mpsc::Receiver<CatalogCommand>,
}

impl CatalogWorker {
    pub fn new(
        catalog: Arc<Catalog>,
        root: Arc<PathBuf>,
        blacklist: Arc<HashSet<String>>,
        interval_secs: u64,
        rx: mpsc::Receiver<CatalogCommand>,
    ) -> Self {
        let clamped = interval_secs.max(1);
        Self {
            catalog,
            root,
            blacklist,
            interval: Duration::from_secs(clamped),
            rx,
        }
    }

    pub async fn run(mut self) {
        let mut ticker = time::interval(self.interval);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(err) = self.catalog.refresh_full(&self.root, &self.blacklist).await {
                        tracing::error!("Catalog refresh failed: {:?}", err);
                    }
                }
                command = self.rx.recv() => {
                    match command {
                        Some(CatalogCommand::RefreshAll) => {
                            if let Err(err) = self.catalog.refresh_full(&self.root, &self.blacklist).await {
                                tracing::error!("Catalog refresh failed: {:?}", err);
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    }
}
