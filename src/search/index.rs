use anyhow::{Context, Result, bail};
use directories::UserDirs;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Statement, TransactionBehavior, params};
use std::collections::HashSet;
#[cfg(windows)]
use std::env;
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use walkdir::{DirEntry, WalkDir};

use super::{
    embedding::{EmbeddingProfile, Embeddings},
    vector_store::{self, VectorBuilder},
};

const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
const CHUNK_CHARS: usize = 900;
const CHUNK_OVERLAP: usize = 120;
pub const MAX_QUERY_CHARS: usize = 256;
const SCHEMA_VERSION: i64 = 2;
const EMBEDDING_BATCH: usize = 32;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS entries (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL,
        path_key TEXT NOT NULL,
        title TEXT NOT NULL,
        name_key TEXT NOT NULL,
        aliases TEXT NOT NULL,
        kind INTEGER NOT NULL,
        modified INTEGER NOT NULL,
        UNIQUE (kind, path_key)
    );
    CREATE INDEX IF NOT EXISTS entry_kind ON entries(kind);
    CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
        terms,
        body UNINDEXED,
        entry_id UNINDEXED,
        ordinal UNINDEXED,
        tokenize = 'unicode61 remove_diacritics 2'
    );
    CREATE TABLE IF NOT EXISTS vector_snapshot (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        table_version INTEGER,
        dimensions INTEGER,
        profile TEXT NOT NULL,
        roots TEXT NOT NULL
    );
";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchMode {
    #[default]
    All,
    Apps,
    Files,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    App,
    File,
    Folder,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub path: PathBuf,
    pub detail: String,
    pub excerpt: String,
    pub kind: HitKind,
    /// A one-based chunk number, present for text hits.
    pub chunk: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexSummary {
    pub files: usize,
    pub apps: usize,
    pub chunks: usize,
    pub vectors: usize,
    pub embedding: Option<String>,
    pub semantic: bool,
}

struct VectorSnapshot {
    table_version: Option<i64>,
    dimensions: Option<usize>,
    profile: EmbeddingProfile,
    roots: Vec<PathBuf>,
}

pub fn default_roots() -> Vec<PathBuf> {
    let Some(dirs) = UserDirs::new() else {
        return Vec::new();
    };
    let mut roots = [dirs.desktop_dir(), dirs.document_dir(), dirs.download_dir()]
        .into_iter()
        .flatten()
        .filter(|path| path.is_dir())
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

pub fn summary(database: &Path) -> Result<IndexSummary> {
    let Some(mut connection) = read_database(database)? else {
        return Ok(IndexSummary::default());
    };
    let tx = connection.transaction()?;
    summary_in(&tx, read_vector_snapshot(&tx)?.as_ref())
}

fn summary_in(connection: &Connection, snapshot: Option<&VectorSnapshot>) -> Result<IndexSummary> {
    let (files, apps, chunks) = connection.query_row(
        "SELECT (SELECT count(*) FROM entries WHERE kind = 0),
                (SELECT count(*) FROM entries WHERE kind = 1),
                (SELECT count(*) FROM chunks)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(IndexSummary {
        files,
        apps,
        chunks,
        vectors: if snapshot.is_some_and(|snapshot| snapshot.table_version.is_some()) {
            chunks
        } else {
            0
        },
        embedding: snapshot.map(|snapshot| snapshot.profile.label()),
        semantic: snapshot.is_some_and(|snapshot| !snapshot.profile.is_lexical()),
    })
}

pub fn vector_directory(database: &Path) -> PathBuf {
    database.with_extension("lancedb")
}

/// Keep a completed index on normal startup and migrate old indexes on first open.
pub fn prepare(database: &Path, roots: &[PathBuf]) -> Result<IndexSummary> {
    if let Some(mut connection) = read_database(database)? {
        let tx = connection.transaction()?;
        if let Some(snapshot) = read_vector_snapshot(&tx)? {
            let absolute_roots = roots
                .iter()
                .map(std::path::absolute)
                .collect::<std::io::Result<Vec<_>>>()?;
            let model = Embeddings::load(database.parent().unwrap_or_else(|| Path::new(".")))?;
            if snapshot.roots == absolute_roots
                && &snapshot.profile == model.profile()
                && (snapshot.table_version.is_none() || vector_directory(database).is_dir())
            {
                return summary_in(&tx, Some(&snapshot));
            }
        }
    }
    rebuild(database, roots)
}

pub fn rebuild(database: &Path, roots: &[PathBuf]) -> Result<IndexSummary> {
    let database = std::path::absolute(database)?;
    let parent = database.parent().context("本地索引缺少父目录")?;
    fs::create_dir_all(parent)?;
    let model = Embeddings::load(parent)?;
    let roots = roots
        .iter()
        .map(std::path::absolute)
        .collect::<std::io::Result<Vec<_>>>()?;
    let lance = vector_directory(&database);
    let mut connection = Connection::open(&database)
        .with_context(|| format!("无法打开本地索引 {}", database.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;

    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = schema_version(&tx)?;
    if version > SCHEMA_VERSION {
        bail!("本地索引版本 {version} 高于当前程序支持的版本 {SCHEMA_VERSION}");
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.execute_batch("DELETE FROM chunks; DELETE FROM entries;")?;

    let mut vectors = VectorBuilder::new()?;
    let mut pending = Vec::with_capacity(EMBEDDING_BATCH);
    let mut dimensions = None;
    let mut counts = {
        let mut entries = tx.prepare(
            "INSERT INTO entries (path, path_key, title, name_key, aliases, kind, modified)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (kind, path_key) DO NOTHING
                 RETURNING id",
        )?;
        let mut chunks = tx.prepare(
            "INSERT INTO chunks (rowid, terms, body, entry_id, ordinal) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut counts = IndexSummary::default();
        counts.apps = add_apps(&mut entries)?;

        let database_key = path_key(&database).unwrap_or_default();
        let database_files = [
            format!("{database_key}-wal"),
            format!("{database_key}-shm"),
            format!("{database_key}-journal"),
            database_key,
        ];
        let lance_key = path_key(&lance);
        for root in &roots {
            for entry in WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    should_walk(entry)
                        && lance_key
                            .as_ref()
                            .is_none_or(|key| path_key(entry.path()).as_ref() != Some(key))
                })
                .filter_map(Result::ok)
            {
                let path = entry.path();
                if !entry.file_type().is_file()
                    || path_key(path).is_some_and(|key| database_files.contains(&key))
                {
                    continue;
                }

                let title = entry.file_name().to_string_lossy();
                let timestamp = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |time| time.as_secs() as i64);
                let Some(id) = add_entry(&mut entries, path, &title, "", 0, timestamp)? else {
                    continue;
                };
                counts.files += 1;
                if let Some(body) = read_text(path) {
                    for (ordinal, chunk) in text_chunks(&body).enumerate() {
                        let row_id = i64::try_from(counts.chunks + 1)?;
                        chunks.execute(params![
                            row_id,
                            fts_terms(chunk),
                            chunk,
                            id,
                            ordinal + 1
                        ])?;
                        pending.push((row_id, chunk.to_owned()));
                        if pending.len() == EMBEDDING_BATCH {
                            add_embeddings(&model, &mut vectors, &mut pending, &mut dimensions)?;
                        }
                        counts.chunks += 1;
                    }
                }
            }
        }
        counts
    };
    add_embeddings(&model, &mut vectors, &mut pending, &mut dimensions)?;
    let table_version = if counts.chunks == 0 {
        None
    } else {
        Some(vectors.finish(&lance)?)
    };
    tx.execute(
        "INSERT OR REPLACE INTO vector_snapshot (singleton, table_version, dimensions, profile, roots)
         VALUES (1, ?1, ?2, ?3, ?4)",
        params![
            table_version,
            dimensions,
            serde_json::to_string(model.profile())?,
            serde_json::to_string(&roots)?
        ],
    )?;
    tx.commit()?;
    counts.vectors = counts.chunks;
    counts.embedding = Some(model.profile().label());
    counts.semantic = !model.profile().is_lexical();
    Ok(counts)
}

fn add_embeddings(
    model: &Embeddings,
    index: &mut VectorBuilder,
    pending: &mut Vec<(i64, String)>,
    dimensions: &mut Option<usize>,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let vectors = model.embed(
        &pending
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
    )?;
    for ((row, _), vector) in pending.drain(..).zip(vectors) {
        if *dimensions.get_or_insert(vector.len()) != vector.len() {
            bail!("嵌入模型在构建时改变了向量维数，请检查模型配置");
        }
        index.insert(row, vector)?;
    }
    Ok(())
}

fn read_vector_snapshot(connection: &Connection) -> Result<Option<VectorSnapshot>> {
    if schema_version(connection)? < 2 {
        return Ok(None);
    }
    let row = connection.query_row(
        "SELECT table_version, dimensions, profile, roots FROM vector_snapshot WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
    ).optional()?;
    row.map(|(table_version, dimensions, profile, roots)| {
        Ok(VectorSnapshot {
            table_version,
            dimensions,
            profile: serde_json::from_str(&profile).context("无法读取向量库模型配置")?,
            roots: serde_json::from_str(&roots).context("无法读取向量库目录设置")?,
        })
    })
    .transpose()
}

pub fn search(database: &Path, query: &str, mode: SearchMode, limit: usize) -> Result<Vec<Hit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let Some(mut connection) = read_database(database)? else {
        return Ok(Vec::new());
    };
    // Mixed results should all describe the same snapshot, even during a rebuild.
    let tx = connection.transaction()?;
    let query = bounded_query(query);
    let mut hits = match mode {
        SearchMode::All if query.is_empty() => name_hits(&tx, &query, Some(1), limit)?,
        SearchMode::All => name_hits(&tx, &query, None, limit)?,
        SearchMode::Apps => name_hits(&tx, &query, Some(1), limit)?,
        SearchMode::Files => name_hits(&tx, &query, Some(0), limit)?,
        SearchMode::Text => Vec::new(),
    };
    if matches!(mode, SearchMode::All | SearchMode::Text) && hits.len() < limit {
        let remaining = limit - hits.len();
        match retrieve_in(&tx, database, &query, remaining) {
            Ok(passages) => hits.extend(passages),
            Err(_) if !hits.is_empty() => {}
            Err(error) => return Err(error),
        }
    }
    Ok(hits)
}

/// Query only the filename index. Used when an external filename source is
/// merged with Moin's own index.
pub fn search_names(
    database: &Path,
    query: &str,
    mode: SearchMode,
    limit: usize,
) -> Result<Vec<Hit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let kind = match mode {
        SearchMode::Apps => Some(1),
        SearchMode::Files => Some(0),
        SearchMode::All => None,
        SearchMode::Text => return Ok(Vec::new()),
    };
    let Some(mut connection) = read_database(database)? else {
        return Ok(Vec::new());
    };
    let tx = connection.transaction()?;
    name_hits(&tx, &bounded_query(query), kind, limit)
}

/// Query the committed LanceDB generation together with exact text matches.
pub fn retrieve(database: &Path, query: &str, limit: usize) -> Result<Vec<Hit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let Some(mut connection) = read_database(database)? else {
        return Ok(Vec::new());
    };
    let tx = connection.transaction()?;
    retrieve_in(&tx, database, &bounded_query(query), limit)
}

fn retrieve_in(
    connection: &Connection,
    database: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<Hit>> {
    let Some(strict) = strict_expression(&query) else {
        return Ok(Vec::new());
    };
    let mut hits = text_hits(connection, &strict, limit)?;
    if hits.len() == limit {
        return Ok(hits);
    }
    let vector_error = match vector_hits(connection, database, query, limit) {
        Ok(vectors) => {
            append_unique(&mut hits, vectors, limit);
            None
        }
        Err(error) => Some(error),
    };
    if hits.len() < limit
        && let Some(broad) = overlap_expression(&query)
        && broad != strict
    {
        append_unique(&mut hits, text_hits(connection, &broad, limit)?, limit);
    }
    if hits.is_empty()
        && let Some(error) = vector_error
    {
        return Err(error);
    }
    Ok(hits)
}

fn append_unique(hits: &mut Vec<Hit>, candidates: Vec<Hit>, limit: usize) {
    let mut seen = hits
        .iter()
        .map(|hit| (hit.path.clone(), hit.chunk))
        .collect::<HashSet<_>>();
    for hit in candidates {
        if hits.len() == limit {
            break;
        }
        if seen.insert((hit.path.clone(), hit.chunk)) {
            hits.push(hit);
        }
    }
}

fn vector_hits(
    connection: &Connection,
    database: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<Hit>> {
    let Some(snapshot) = read_vector_snapshot(connection)? else {
        return Ok(Vec::new());
    };
    let Some(version) = snapshot.table_version else {
        return Ok(Vec::new());
    };
    let lexical = snapshot.profile.is_lexical();
    let model = Embeddings::from_profile(snapshot.profile)?;
    let embeddings = model.embed(&[query])?;
    let vector = embeddings.first().context("向量模型没有返回查询向量")?;
    if Some(vector.len()) != snapshot.dimensions {
        bail!("查询模型的向量维度已改变，请更新索引");
    }
    let results = vector_store::search(&vector_directory(database), version, vector, limit)?;
    let threshold = if lexical { 0.12 } else { 0.25 };
    let mut row = connection.prepare(
        "SELECT entries.path, entries.title, chunks.body, chunks.ordinal
         FROM chunks JOIN entries ON entries.id = chunks.entry_id
         WHERE chunks.rowid = ?1",
    )?;
    let mut hits = Vec::new();
    for (id, similarity) in results {
        if similarity < threshold {
            continue;
        }
        let hit = row
            .query_row([id], |row| {
                let path = PathBuf::from(row.get::<_, String>(0)?);
                Ok(Hit {
                    detail: format!("向量匹配 · {}", directory_label(&path)),
                    path,
                    title: row.get(1)?,
                    excerpt: row.get(2)?,
                    kind: HitKind::Text,
                    chunk: Some(row.get(3)?),
                })
            })
            .optional()?;
        if let Some(hit) = hit {
            hits.push(hit);
        }
    }
    Ok(hits)
}

fn schema_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn read_database(database: &Path) -> Result<Option<Connection>> {
    if !database.try_exists()? {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let version = schema_version(&connection)?;
    if version > SCHEMA_VERSION {
        bail!("本地索引版本 {version} 高于当前程序支持的版本 {SCHEMA_VERSION}");
    }
    Ok((version >= 1).then_some(connection))
}

fn name_hits(
    connection: &Connection,
    query: &str,
    kind: Option<i64>,
    limit: usize,
) -> Result<Vec<Hit>> {
    let mut statement = connection.prepare(
        "SELECT path, title, kind FROM entries
         WHERE (?2 IS NULL OR kind = ?2)
           AND (?1 = '' OR instr(name_key, ?1) > 0 OR instr(aliases, ?1) > 0
                OR (?4 AND instr(lower(path_key), ?1) > 0))
         ORDER BY CASE
                    WHEN name_key = ?1 OR aliases = ?1 THEN 0
                    WHEN instr(name_key, ?1) = 1 THEN 1
                    WHEN instr(name_key, ?1) > 0 THEN 2
                    ELSE 3
                  END,
                  kind DESC,
                  CASE WHEN ?1 = '' AND kind = 0 THEN modified ELSE 0 END DESC,
                  length(name_key), name_key, path_key
         LIMIT ?3",
    )?;
    let query = if cfg!(windows) {
        query.to_lowercase().replace('/', "\\")
    } else {
        query.to_lowercase()
    };
    let rows = statement.query_map(
        params![
            query,
            kind,
            sqlite_limit(limit),
            query.contains(['/', '\\', ':'])
        ],
        |row| {
            let path = PathBuf::from(row.get::<_, String>(0)?);
            Ok(Hit {
                detail: directory_label(&path),
                path,
                title: row.get(1)?,
                excerpt: String::new(),
                kind: if row.get::<_, i64>(2)? == 1 {
                    HitKind::App
                } else {
                    HitKind::File
                },
                chunk: None,
            })
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn text_hits(connection: &Connection, expression: &str, limit: usize) -> Result<Vec<Hit>> {
    let mut statement = connection.prepare(
        "SELECT entries.path, entries.title, chunks.body, chunks.ordinal
         FROM chunks JOIN entries ON entries.id = chunks.entry_id
         WHERE chunks MATCH ?1
         ORDER BY chunks.rank, entries.path_key, chunks.ordinal
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![expression, sqlite_limit(limit)], |row| {
        let path = PathBuf::from(row.get::<_, String>(0)?);
        Ok(Hit {
            detail: directory_label(&path),
            path,
            title: row.get(1)?,
            excerpt: row.get(2)?,
            kind: HitKind::Text,
            chunk: Some(row.get(3)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn sqlite_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn directory_label(path: &Path) -> String {
    path.parent()
        .map_or_else(String::new, |parent| parent.display().to_string())
}

fn path_key(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    Some(if cfg!(windows) {
        path.replace('/', "\\").to_lowercase()
    } else {
        path.to_owned()
    })
}

fn add_entry(
    statement: &mut Statement<'_>,
    path: &Path,
    title: &str,
    aliases: &str,
    kind: i64,
    modified: i64,
) -> Result<Option<i64>> {
    let Some(path_key) = path_key(path) else {
        return Ok(None);
    };
    let mut rows = statement.query(params![
        path.to_str(),
        path_key,
        title,
        title.to_lowercase(),
        aliases.to_lowercase(),
        kind,
        modified
    ])?;
    Ok(rows.next()?.map(|row| row.get(0)).transpose()?)
}

#[cfg(windows)]
fn add_apps(statement: &mut Statement<'_>) -> Result<usize> {
    let mut count = 0;
    let mut known_titles = HashSet::new();
    for directory in ["APPDATA", "PROGRAMDATA"]
        .into_iter()
        .filter_map(env::var_os)
    {
        let programs = PathBuf::from(directory).join("Microsoft/Windows/Start Menu/Programs");
        for entry in WalkDir::new(programs)
            .max_depth(8)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !entry.file_type().is_file()
                || !path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("lnk") || ext.eq_ignore_ascii_case("appref-ms")
                    })
            {
                continue;
            }
            let Some(title) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if add_entry(statement, path, title, "", 1, 0)?.is_some() {
                known_titles.insert(title.to_lowercase());
                count += 1;
            }
        }
    }

    if let Some(root) = env::var_os("SystemRoot") {
        let root = PathBuf::from(root);
        let built_ins = [
            ("explorer.exe", "文件资源管理器", "File Explorer explorer"),
            ("System32/notepad.exe", "记事本", "notepad"),
            ("System32/calc.exe", "计算器", "calculator calc"),
            ("System32/mspaint.exe", "画图", "Microsoft Paint mspaint"),
            ("System32/Taskmgr.exe", "任务管理器", "Task Manager taskmgr"),
        ];
        for (executable, title, aliases) in built_ins {
            let path = root.join(executable);
            if path.is_file()
                && !known_titles.contains(&title.to_lowercase())
                && add_entry(statement, &path, title, aliases, 1, 0)?.is_some()
            {
                count += 1;
            }
        }
    }
    Ok(count)
}

#[cfg(target_os = "macos")]
fn add_apps(statement: &mut Statement<'_>) -> Result<usize> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Library/CoreServices/Applications"),
        PathBuf::from("/System/Library/CoreServices/Finder.app"),
    ];
    if let Some(dirs) = UserDirs::new() {
        roots.push(dirs.home_dir().join("Applications"));
    }
    add_macos_apps(statement, &roots)
}

/// Treat an application bundle as one launchable item, including symlinked apps.
/// Never descend into its Resources, frameworks, or bundled helper applications.
#[cfg(any(target_os = "macos", test))]
fn add_macos_apps(statement: &mut Statement<'_>, roots: &[PathBuf]) -> Result<usize> {
    let mut count = 0;
    let mut known_paths = HashSet::new();
    for root in roots {
        let mut walker = WalkDir::new(root)
            .follow_links(false)
            .max_depth(8)
            .sort_by_file_name()
            .into_iter();
        while let Some(entry) = walker.next() {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if is_app_bundle_path(path) && path.is_dir() {
                // WalkDir follows root directory symlinks even with follow_links(false).
                // A non-root symlink is not on the traversal stack and must not pop it.
                if entry.file_type().is_dir() || entry.depth() == 0 {
                    walker.skip_current_dir();
                }
                let info = path.join("Contents/Info.plist");
                if !info.is_file() {
                    continue;
                }
                let Ok(canonical) = path.canonicalize() else {
                    continue;
                };
                if !known_paths.insert(canonical) {
                    continue;
                }
                let Some((title, aliases)) = macos_bundle_labels(path, &info) else {
                    continue;
                };
                let modified = info
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |time| time.as_secs() as i64);
                if add_entry(statement, path, &title, &aliases, 1, modified)?.is_some() {
                    count += 1;
                }
            } else if !should_walk(&entry) {
                walker.skip_current_dir();
            }
        }
    }
    Ok(count)
}

#[cfg(any(target_os = "macos", test))]
fn macos_bundle_labels(path: &Path, info: &Path) -> Option<(String, String)> {
    let fallback = path.file_stem()?.to_str()?.to_owned();
    // Corrupt or missing metadata must not prevent launching an otherwise usable app.
    // Bound the read since application bundles may reside in user-controlled folders.
    let metadata = (|| -> Option<plist::Value> {
        let file = File::open(info).ok()?;
        const LIMIT: u64 = 1024 * 1024;
        if file.metadata().ok()?.len() > LIMIT {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > LIMIT {
            return None;
        }
        plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()
    })();
    let string = |key: &str| -> Option<&str> {
        metadata
            .as_ref()?
            .as_dictionary()?
            .get(key)?
            .as_string()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let title = string("CFBundleDisplayName")
        .or_else(|| string("CFBundleName"))
        .unwrap_or(&fallback)
        .to_owned();
    let mut aliases = vec![fallback.as_str()];
    aliases.extend(
        [
            "CFBundleName",
            "CFBundleDisplayName",
            "CFBundleIdentifier",
            "CFBundleExecutable",
        ]
        .into_iter()
        .filter_map(string),
    );
    Some((title, aliases.join(" ")))
}

fn is_app_bundle_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn add_apps(_statement: &mut Statement<'_>) -> Result<usize> {
    Ok(0)
}

fn should_walk(entry: &DirEntry) -> bool {
    should_walk_files(entry, cfg!(target_os = "macos"))
}

fn should_walk_files(entry: &DirEntry, skip_app_bundles: bool) -> bool {
    if skip_app_bundles && is_app_bundle_path(entry.path()) && entry.path().is_dir() {
        return false;
    }
    entry.depth() == 0
        || !entry.file_type().is_dir()
        || !matches!(
            entry.file_name().to_str(),
            Some(".git" | ".svn" | ".hg" | "node_modules" | "target" | ".venv" | "__pycache__")
        )
}

fn read_text(path: &Path) -> Option<String> {
    if !is_text_file(path) {
        return None;
    }
    let metadata = path.metadata().ok()?;
    if metadata.len() > MAX_TEXT_BYTES || is_cloud_placeholder(&metadata) {
        return None;
    }

    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return None;
    }
    let is_little_endian = bytes.starts_with(&[0xff, 0xfe]);
    let body = if is_little_endian || bytes.starts_with(&[0xfe, 0xff]) {
        let words = bytes[2..].chunks_exact(2);
        if !words.remainder().is_empty() {
            return None;
        }
        let words = words.map(|bytes| {
            let pair = [bytes[0], bytes[1]];
            if is_little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        });
        char::decode_utf16(words)
            .collect::<Result<String, _>>()
            .ok()?
    } else {
        String::from_utf8(bytes).ok()?
    };
    let body = body.strip_prefix('\u{feff}').unwrap_or(&body);
    (!body.contains('\0')).then(|| body.to_owned())
}

#[cfg(windows)]
fn is_cloud_placeholder(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x40000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    metadata.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(not(windows))]
fn is_cloud_placeholder(_metadata: &Metadata) -> bool {
    false
}

fn is_text_file(path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|name| name.to_str())
        && ["readme", "license", "makefile", "dockerfile", ".gitignore"]
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
    {
        return true;
    }
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    [
        "txt",
        "md",
        "markdown",
        "rst",
        "adoc",
        "org",
        "text",
        "log",
        "csv",
        "tsv",
        "json",
        "jsonl",
        "jsonc",
        "yaml",
        "yml",
        "toml",
        "xml",
        "html",
        "htm",
        "css",
        "scss",
        "ini",
        "cfg",
        "conf",
        "properties",
        "tex",
        "rs",
        "py",
        "js",
        "jsx",
        "ts",
        "tsx",
        "vue",
        "svelte",
        "c",
        "cc",
        "cpp",
        "h",
        "hpp",
        "cs",
        "java",
        "kt",
        "swift",
        "go",
        "rb",
        "php",
        "lua",
        "sh",
        "bash",
        "zsh",
        "fish",
        "ps1",
        "bat",
        "cmd",
        "sql",
    ]
    .contains(&extension.as_str())
}

fn text_chunks(mut rest: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let Some((limit, _)) = rest.char_indices().nth(CHUNK_CHARS) else {
            return Some(std::mem::take(&mut rest));
        };
        let end = rest[..limit]
            .char_indices()
            .rev()
            .take(160)
            .find_map(|(offset, ch)| {
                (ch.is_whitespace() || matches!(ch, '。' | '！' | '？'))
                    .then_some(offset + ch.len_utf8())
            })
            .unwrap_or(limit);
        let chunk = &rest[..end];
        let (next, _) = chunk
            .char_indices()
            .rev()
            .nth(CHUNK_OVERLAP - 1)
            .expect("a full chunk exceeds the overlap");
        rest = &rest[next..];
        Some(chunk)
    })
    .map(str::trim)
    .filter(|text| text.chars().any(char::is_alphanumeric))
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3041}'..='\u{3096}' | '\u{30A1}'..='\u{30FA}' | '\u{3400}'..='\u{4DBF}' |
        '\u{4E00}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{F900}'..='\u{FAFF}' |
        '\u{20000}'..='\u{2FA1F}' | '\u{30000}'..='\u{323AF}'
    )
}

// CJK phrases use one token per character. Their FTS positions retain word order.
fn fts_terms(text: &str) -> String {
    let mut terms = String::with_capacity(text.len());
    for ch in text.chars() {
        if is_cjk(ch) {
            terms.push(' ');
            terms.push(ch);
            terms.push(' ');
        } else {
            terms.push(ch);
        }
    }
    terms
}

fn bounded_query(query: &str) -> String {
    query.trim().chars().take(MAX_QUERY_CHARS).collect()
}

fn quoted_phrase(term: &str) -> String {
    format!("\"{}\"", fts_terms(term).replace('"', "\"\""))
}

fn strict_expression(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .take(32)
        .map(|term| {
            let mut phrase = quoted_phrase(term);
            if term.ends_with(|ch: char| ch.is_ascii_alphanumeric()) {
                phrase.push('*');
            }
            phrase
        })
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

fn overlap_expression(query: &str) -> Option<String> {
    let spaced = fts_terms(query);
    let words = spaced.split_whitespace().collect::<Vec<_>>();
    let mut phrases = HashSet::new();
    for (offset, word) in words.iter().enumerate() {
        if word.starts_with(is_cjk) {
            if let Some(next) = words
                .get(offset + 1)
                .filter(|next| next.starts_with(is_cjk))
            {
                phrases.insert(quoted_phrase(&format!("{word}{next}")));
            }
        } else {
            let word = word.trim_matches(|ch: char| !ch.is_alphanumeric());
            if word.chars().count() > 2 {
                phrases.insert(quoted_phrase(word));
            }
        }
    }
    let mut phrases = phrases.into_iter().collect::<Vec<_>>();
    phrases.sort_unstable();
    phrases.truncate(64);
    (phrases.len() > 1).then(|| phrases.join(" OR "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };
    use tempfile::tempdir;

    #[test]
    fn discovers_macos_bundles_without_indexing_nested_helpers() -> Result<()> {
        let temp = tempdir()?;
        let apps = temp.path().join("Applications");
        let app = apps.join("Tools/Example.app");
        fs::create_dir_all(app.join("Contents/Resources/Helper.app/Contents"))?;
        fs::write(
            app.join("Contents/Resources/Helper.app/Contents/Info.plist"),
            "invalid",
        )?;
        let mut info = plist::Dictionary::new();
        info.insert("CFBundleDisplayName".into(), "示例应用".into());
        info.insert("CFBundleName".into(), "Example Tool".into());
        info.insert("CFBundleIdentifier".into(), "dev.moin.example".into());
        info.insert("CFBundleExecutable".into(), "example-binary".into());
        // Most built-in apps ship binary property lists; exercise that format too.
        plist::Value::Dictionary(info).to_file_binary(app.join("Contents/Info.plist"))?;
        let broken = apps.join("Broken.app");
        fs::create_dir_all(broken.join("Contents"))?;
        fs::write(broken.join("Contents/Info.plist"), "corrupt metadata")?;
        fs::create_dir_all(apps.join("Ordinary.app"))?;

        let connection = Connection::open_in_memory()?;
        connection.execute_batch(SCHEMA)?;
        let mut entries = connection.prepare(
            "INSERT INTO entries (path, path_key, title, name_key, aliases, kind, modified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (kind, path_key) DO NOTHING RETURNING id",
        )?;
        assert_eq!(
            add_macos_apps(&mut entries, &[apps.clone(), app.clone()])?,
            2
        );
        drop(entries);
        let found = name_hits(&connection, "dev.moin.example", Some(1), 10)?;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, app);
        assert_eq!(found[0].title, "示例应用");
        assert_eq!(found[0].kind, HitKind::App);
        assert_eq!(
            name_hits(&connection, "example-binary", Some(1), 10)?.len(),
            1
        );
        assert_eq!(name_hits(&connection, "Example.app", Some(1), 10)?.len(), 0);
        assert_eq!(name_hits(&connection, "Helper", Some(1), 10)?.len(), 0);
        assert_eq!(name_hits(&connection, "Broken", Some(1), 10)?.len(), 1);
        assert_eq!(
            name_hits(&connection, "Applications/Tools/Example.app", Some(1), 10)?.len(),
            1
        );
        Ok(())
    }

    #[test]
    fn macos_file_walk_prunes_application_contents() -> Result<()> {
        let temp = tempdir()?;
        let bundle = temp.path().join("Example.app");
        fs::create_dir_all(bundle.join("Contents"))?;
        fs::write(bundle.join("Contents/private.txt"), "not a search document")?;
        fs::write(temp.path().join("notes.txt"), "search document")?;
        let files = WalkDir::new(temp.path())
            .into_iter()
            .filter_entry(|entry| should_walk_files(entry, true))
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.file_name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(files, vec![std::ffi::OsString::from("notes.txt")]);
        assert!(
            WalkDir::new(&bundle)
                .into_iter()
                .filter_entry(|entry| should_walk_files(entry, true))
                .next()
                .is_none()
        );
        #[cfg(unix)]
        {
            let link = temp.path().join("Linked.app");
            std::os::unix::fs::symlink(&bundle, &link)?;
            assert!(
                WalkDir::new(link)
                    .into_iter()
                    .filter_entry(|entry| should_walk_files(entry, true))
                    .next()
                    .is_none()
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_macos_app_keeps_scanning_sibling_apps() -> Result<()> {
        let temp = tempdir()?;
        let apps = temp.path().join("Applications");
        let target = temp.path().join("Actual.app");
        fs::create_dir_all(&apps)?;
        fs::create_dir_all(target.join("Contents"))?;
        fs::write(target.join("Contents/Info.plist"), "fallback to filename")?;
        let helper = target.join("Contents/Helper.app/Contents");
        fs::create_dir_all(&helper)?;
        fs::write(
            helper.join("Info.plist"),
            "nested helper is not a separate app",
        )?;
        std::os::unix::fs::symlink(&target, apps.join("A.app"))?;
        let second = apps.join("B.app");
        fs::create_dir_all(second.join("Contents"))?;
        fs::write(second.join("Contents/Info.plist"), "fallback to filename")?;
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(SCHEMA)?;
        let mut entries = connection.prepare(
            "INSERT INTO entries (path, path_key, title, name_key, aliases, kind, modified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (kind, path_key) DO NOTHING RETURNING id",
        )?;
        assert_eq!(add_macos_apps(&mut entries, &[apps.clone()])?, 2);
        assert_eq!(add_macos_apps(&mut entries, &[apps.join("A.app")])?, 0);
        Ok(())
    }

    #[test]
    fn searches_names_and_chinese_text_across_rebuilds() -> Result<()> {
        let temp = tempdir()?;
        let database = temp.path().join("index.sqlite3");
        let docs = temp.path().join("docs");
        fs::create_dir(&docs)?;
        let budget = docs.join("年度报告.md");
        fs::write(
            &budget,
            "项目预算需要在十月底冻结。\nThe revised cost is fixed.",
        )?;
        fs::write(docs.join("image.png"), "项目预算")?;
        let roots = [docs.clone(), docs];

        assert_eq!(summary(&database)?, IndexSummary::default());
        let indexed = rebuild(&database, &roots)?;
        assert_eq!(indexed.files, 2);
        assert_eq!(indexed.chunks, 1);
        assert_eq!(indexed.vectors, 1);
        assert_eq!(summary(&database)?, indexed);

        let file = &search(&database, "报告", SearchMode::Files, 10)?[0];
        assert_eq!(file.path, budget);
        assert_eq!(file.kind, HitKind::File);
        let hits = search(&database, "预算 \"revised\"", SearchMode::Text, 10)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HitKind::Text);
        assert_eq!(hits[0].chunk, Some(1));
        assert!(hits[0].excerpt.contains("十月底"));
        assert_eq!(
            retrieve(&database, "如何查询项目预算和成本", 5)?[0].path,
            budget
        );

        fs::remove_file(budget)?;
        rebuild(&database, &roots)?;
        assert!(search(&database, "预算", SearchMode::All, 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn utf16_text_is_chunked_without_breaking_unicode() -> Result<()> {
        let temp = tempdir()?;
        let document = temp.path().join("utf16.txt");
        let original = "世界。".repeat(350) + "结尾";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(original.encode_utf16().flat_map(u16::to_le_bytes));
        fs::write(&document, bytes)?;
        let text = read_text(&document).context("expected UTF-16 text")?;
        let chunks = text_chunks(&text).collect::<Vec<_>>();
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= CHUNK_CHARS)
        );
        assert!(chunks[1].ends_with("结尾"));
        assert!(chunks[0].ends_with(&chunks[1].chars().take(CHUNK_OVERLAP).collect::<String>()));
        Ok(())
    }

    #[test]
    fn migrates_legacy_index_and_keeps_committed_data_on_failure() -> Result<()> {
        let temp = tempdir()?;
        let database = temp.path().join("index.sqlite3");
        let docs = temp.path().join("docs");
        fs::create_dir(&docs)?;
        fs::write(docs.join("旧资料.md"), "资料保留原文内容")?;
        let connection = Connection::open(&database)?;
        connection.execute_batch(SCHEMA)?;
        connection.execute_batch("DROP TABLE vector_snapshot; PRAGMA user_version=1;")?;
        drop(connection);
        let result = prepare(&database, &[docs.clone()])?;
        assert_eq!(result.vectors, 1);
        let version = Connection::open(&database)?.query_row(
            "SELECT table_version FROM vector_snapshot",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(prepare(&database, &[docs.clone()])?, result);
        assert_eq!(
            Connection::open(&database)?.query_row(
                "SELECT table_version FROM vector_snapshot",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            version
        );
        fs::write(temp.path().join("settings.json"), "{\"embedding\":")?;
        assert!(rebuild(&database, &[docs]).is_err());
        assert_eq!(summary(&database)?, result);
        assert!(!retrieve(&database, "资料", 5)?.is_empty());
        Ok(())
    }

    #[test]
    fn semantic_vectors_find_different_wording_and_remove_deleted_passages() -> Result<()> {
        let temp = tempdir()?;
        let database = temp.path().join("index.sqlite3");
        let docs = temp.path().join("documents");
        fs::create_dir(&docs)?;
        let file = docs.join("账户帮助.md");
        fs::write(&file, "忘记登录口令时，可以在账户中心重置密码。")?;
        fs::write(docs.join("天气.txt"), "明天多云，注意携带雨伞。")?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}/api/embed", listener.local_addr()?);
        fs::write(
            temp.path().join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "embedding": {
                    "model": "test-semantic",
                    "endpoint": endpoint,
                }
            }))?,
        )?;
        let running = Arc::new(AtomicBool::new(true));
        let server_running = running.clone();
        let server = thread::spawn(move || -> Result<()> {
            while server_running.load(Ordering::Relaxed) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                let mut input = BufReader::new(stream);
                let mut size = 0;
                loop {
                    let mut line = String::new();
                    if input.read_line(&mut line)? == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("Content-Length")
                    {
                        size = value.trim().parse()?;
                    }
                }
                let mut body = vec![0; size];
                input.read_exact(&mut body)?;
                let request: serde_json::Value = serde_json::from_slice(&body)?;
                let embeddings = request["input"]
                    .as_array()
                    .context("输入数组")?
                    .iter()
                    .map(|text| {
                        if text.as_str().unwrap_or_default().contains("天气")
                            || text.as_str().unwrap_or_default().contains("雨伞")
                        {
                            vec![0.0, 1.0]
                        } else {
                            vec![1.0, 0.0]
                        }
                    })
                    .collect::<Vec<_>>();
                let body = serde_json::to_vec(&serde_json::json!({"embeddings": embeddings}))?;
                write!(
                    input.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )?;
                input.get_mut().write_all(&body)?;
            }
            Ok(())
        });
        let result = (|| -> Result<()> {
            assert!(rebuild(&database, &[docs.clone()])?.semantic);
            let query = "我进不去系统了";
            let hits = search(&database, query, SearchMode::Text, 5)?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].path, file);
            assert_eq!(hits[0].chunk, Some(1));
            assert!(hits[0].detail.starts_with("向量匹配"));
            assert_eq!(retrieve(&database, query, 5)?, hits);
            fs::remove_file(&file)?;
            rebuild(&database, &[docs])?;
            assert!(retrieve(&database, query, 5)?.is_empty());
            Ok(())
        })();
        running.store(false, Ordering::Relaxed);
        server.join().expect("模拟嵌入服务崩溃")?;
        result?;
        let filename_hits = search(&database, "天气.txt", SearchMode::All, 5)?;
        assert_eq!(filename_hits.len(), 1);
        assert_eq!(filename_hits[0].kind, HitKind::File);
        Ok(())
    }
}
