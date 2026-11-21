use super::*;
use rusqlite::{Connection, OpenFlags, params};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct DocHit {
    pub title: String,
    pub kind: String,
    pub docset: String,
    pub version: String,
    pub path: String,
    pub(super) installed: Installed,
}
#[derive(Clone, Debug, Default)]
pub struct SearchReport {
    pub hits: Vec<DocHit>,
    pub warning: Option<String>,
    pub enabled: usize,
}

fn connection(path: &Path, timeout: Duration) -> Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(150))?;
    let deadline = Instant::now() + timeout;
    db.progress_handler(2000, Some(move || Instant::now() >= deadline));
    db.pragma_update(None, "trusted_schema", false)?;
    if db
        .prepare("SELECT name,type,path FROM searchIndex LIMIT 0")
        .is_err()
    {
        // Older Dash/Xcode bundles use a Core Data schema. A temporary projection
        // exposes the same fields without modifying the downloaded database.
        db.execute_batch("CREATE TEMP VIEW searchIndex AS SELECT t.ztokenname name, k.ztypename type, f.zpath path, m.zanchor fragment FROM ztoken t JOIN ztokenmetainformation m ON m.z_pk=t.zmetainformation JOIN zfilepath f ON f.z_pk=m.zfile JOIN ztokentype k ON k.z_pk=t.ztokentype")
            .context("docset 没有受支持的检索索引")?;
    }
    Ok(db)
}
pub(super) fn validate_index(path: &Path) -> Result<u64> {
    let db = connection(path, Duration::from_secs(4))?;
    db.query_row("SELECT COUNT(*) FROM searchIndex", [], |row| row.get(0))
        .context("无法读取 docset 索引")
}
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
fn split_scope<'a>(installed: &'a [Installed], query: &str) -> (Vec<&'a Installed>, String) {
    let split = query.find(|c: char| c == ':' || c.is_whitespace());
    if let Some(index) = split {
        let word = query[..index].to_lowercase();
        let matching = installed
            .iter()
            .filter(|item| item.keywords.contains(&word))
            .collect::<Vec<_>>();
        if !matching.is_empty() {
            let width = query[index..].chars().next().unwrap().len_utf8();
            return (matching, query[index + width..].trim_start().to_owned());
        }
    }
    (
        installed.iter().filter(|item| item.enabled).collect(),
        query.to_owned(),
    )
}
pub(super) fn search(
    root: &Path,
    state: &State,
    query: &str,
    limit: usize,
) -> Result<SearchReport> {
    ensure!(query.len() <= 1024, "搜索内容过长");
    let limit = limit.clamp(1, 100);
    let query = query.trim();
    let (documents, query) = split_scope(&state.installed, query);
    let mut report = SearchReport {
        enabled: state
            .installed
            .iter()
            .filter(|docset| docset.enabled)
            .count(),
        ..Default::default()
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    for document in documents {
        if Instant::now() >= deadline {
            report.warning = Some("部分文档集检索超时，请使用 rust: 等关键词缩小范围".into());
            break;
        }
        let results: Result<Vec<DocHit>> = (|| {
            let bundle = document.bundle_path(root)?;
            let db = connection(
                &bundle.join("Contents/Resources/docSet.dsidx"),
                Duration::from_millis(350),
            )?;
            if query.is_empty() {
                let path = document.homepage.clone().unwrap_or_else(|| {
                    db.query_row(
                        "SELECT path FROM searchIndex ORDER BY name LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or_default()
                });
                return Ok(vec![DocHit {
                    title: document.title.clone(),
                    kind: "文档集".into(),
                    docset: document.title.clone(),
                    version: document.version.clone(),
                    path,
                    installed: document.clone(),
                }]);
            }
            let fragment = if db
                .prepare("SELECT fragment FROM searchIndex LIMIT 0")
                .is_ok()
            {
                "COALESCE(fragment,'')"
            } else {
                "''"
            };
            let pattern = format!("%{}%", escape(&query));
            let prefix = format!("{}%", escape(&query));
            let sql = format!(
                "SELECT name,type,path,{fragment} FROM searchIndex WHERE name LIKE ?1 ESCAPE '\\' ORDER BY CASE WHEN lower(name)=lower(?2) THEN 0 WHEN name LIKE ?3 ESCAPE '\\' THEN 1 ELSE 2 END,length(name),name LIMIT ?4"
            );
            let mut statement = db.prepare(&sql)?;
            let rows = statement
                .query_map(params![pattern, query, prefix, limit as i64], |row| {
                    let path: String = row.get(2)?;
                    let anchor: String = row.get(3)?;
                    Ok(DocHit {
                        title: row.get(0)?,
                        kind: row.get(1)?,
                        docset: document.title.clone(),
                        version: document.version.clone(),
                        path: if anchor.is_empty() {
                            path
                        } else {
                            format!("{}#{anchor}", path.split('#').next().unwrap_or(&path))
                        },
                        installed: document.clone(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !rows.is_empty() {
                return Ok(rows);
            }
            // Subsequence fallback, useful for abbreviated symbol names.
            let fuzzy = format!(
                "%{}%",
                query
                    .chars()
                    .map(|c| escape(&c.to_string()))
                    .collect::<Vec<_>>()
                    .join("%")
            );
            let mut statement=db.prepare(&format!("SELECT name,type,path,{fragment} FROM searchIndex WHERE name LIKE ?1 ESCAPE '\\' ORDER BY length(name),name LIMIT ?2"))?;
            statement
                .query_map(params![fuzzy, limit as i64], |row| {
                    let path: String = row.get(2)?;
                    let anchor: String = row.get(3)?;
                    Ok(DocHit {
                        title: row.get(0)?,
                        kind: row.get(1)?,
                        docset: document.title.clone(),
                        version: document.version.clone(),
                        path: if anchor.is_empty() {
                            path
                        } else {
                            format!("{path}#{anchor}")
                        },
                        installed: document.clone(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })();
        match results {
            Ok(mut hits) => report.hits.append(&mut hits),
            Err(error) => {
                report.warning = Some(format!("{} 暂时无法检索 · {error:#}", document.title))
            }
        }
    }
    let lower = query.to_lowercase();
    report.hits.sort_by_key(|hit| {
        let title = hit.title.to_lowercase();
        (
            if title == lower {
                0
            } else if title.starts_with(&lower) {
                1
            } else if title.contains(&lower) {
                2
            } else {
                3
            },
            title.len(),
            title,
            hit.docset.clone(),
        )
    });
    report
        .hits
        .dedup_by(|a, b| a.title == b.title && a.path == b.path && a.docset == b.docset);
    report.hits.truncate(limit);
    Ok(report)
}

pub(super) fn resolve(root: &Path, hit: &DocHit) -> Result<url::Url> {
    let bundle = hit.installed.bundle_path(root)?;
    let documents = bundle.join("Contents/Resources/Documents").canonicalize()?;
    let clean = super::reader::clean_location(&hit.path);
    let (raw, anchor) = clean.split_once('#').unwrap_or((&clean, ""));
    let (raw, query) = raw.split_once('?').unwrap_or((raw, ""));
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .context("文档路径编码无效")?;
    let relative = download::safe_relative(&decoded)?;
    let path = documents
        .join(relative)
        .canonicalize()
        .context("本地文档不存在，请删除后重新安装此文档集")?;
    ensure!(
        path.starts_with(&documents) && path.is_file(),
        "文档路径超出文档集目录"
    );
    let mut url =
        url::Url::from_file_path(path).map_err(|_| anyhow::anyhow!("无法打开文档路径"))?;
    if !query.is_empty() {
        url.set_query(Some(query));
    }
    if !anchor.is_empty() {
        url.set_fragment(Some(anchor));
    }
    Ok(url)
}
