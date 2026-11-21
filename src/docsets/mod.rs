//! Dash-compatible offline documentation. Catalog and archives come from Kapeli;
//! installations are published only after extraction and SQLite validation.
mod download;
mod reader;
mod search;
mod server;
#[cfg(test)]
mod tests;
use anyhow::{Context, Result, ensure};
pub use reader::{DocPage, LinkTarget};
pub use search::{DocHit, SearchReport};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

pub const CATALOG_SOURCE: &str = "https://github.com/Kapeli/feeds";
const MAX_METADATA: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Available {
    pub key: String,
    pub title: String,
}
impl Available {
    fn from_key(key: String) -> Result<Self> {
        ensure!(
            !key.is_empty()
                && key.len() <= 100
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_+-.() ".contains(c)),
            "文档集标识无效"
        );
        Ok(Self {
            title: key.replace('_', " "),
            key,
        })
    }
    fn hash(&self) -> String {
        Sha256::digest(self.key.as_bytes())[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    pub key: String,
    pub title: String,
    pub version: String,
    pub enabled: bool,
    pub keywords: Vec<String>,
    pub symbols: u64,
    pub size: u64,
    directory: String,
    bundle: PathBuf,
    homepage: Option<String>,
}

impl Installed {
    fn bundle_path(&self, root: &Path) -> Result<PathBuf> {
        ensure!(
            self.directory.len() == 65
                && self.directory.as_bytes()[32] == b'-'
                && self
                    .directory
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| i == 32 || b.is_ascii_hexdigit()),
            "安装目录标识无效"
        );
        let relative = download::safe_relative(&self.bundle.to_string_lossy())?;
        let sets = root.join("sets").canonicalize()?;
        let path = sets
            .join(&self.directory)
            .join(relative)
            .canonicalize()
            .context("文档集文件已丢失，请重新安装")?;
        ensure!(
            path.starts_with(sets.join(&self.directory)),
            "文档集路径无效"
        );
        Ok(path)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub title: String,
    pub stage: String,
    pub received: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct State {
    pub catalog: Vec<Available>,
    pub installed: Vec<Installed>,
    pub progress: Option<Progress>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub loaded: bool,
    pub revision: u64,
}

#[derive(Clone)]
pub struct Manager(Arc<Inner>);
struct Inner {
    root: PathBuf,
    state: watch::Sender<State>,
    busy: AtomicBool,
    cancel: AtomicBool,
    server: std::sync::Mutex<std::collections::HashMap<String, server::Server>>,
}
enum Operation {
    Refresh,
    Install(String),
    Remove(String),
    Enable(String, bool),
}

impl Manager {
    pub fn new(home: &Path) -> Self {
        let bundled: Vec<String> =
            serde_json::from_str(include_str!("../../assets/docsets/catalog.json"))
                .expect("bundled Dash catalog");
        let catalog = bundled
            .into_iter()
            .filter_map(|key| Available::from_key(key).ok())
            .collect();
        let (state, _) = watch::channel(State {
            catalog,
            installed: vec![],
            progress: None,
            error: None,
            notice: None,
            loaded: false,
            revision: 0,
        });
        let this = Self(Arc::new(Inner {
            root: home.join("docsets"),
            state,
            busy: AtomicBool::new(true),
            cancel: AtomicBool::new(false),
            server: std::sync::Mutex::new(std::collections::HashMap::new()),
        }));
        let worker = this.clone();
        std::thread::spawn(move || {
            let result = worker.load();
            worker.0.busy.store(false, Ordering::Release);
            worker.0.state.send_modify(|state| {
                if let Err(error) = result {
                    state.error = Some(format!("读取文档集失败 · {error:#}"));
                }
            });
        });
        this
    }
    pub fn state(&self) -> State {
        self.0.state.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<State> {
        self.0.state.subscribe()
    }
    pub fn refresh_catalog(&self) -> Result<()> {
        self.begin(Operation::Refresh)
    }
    pub fn install(&self, key: &str) -> Result<()> {
        self.begin(Operation::Install(key.into()))
    }
    pub fn remove(&self, key: &str) -> Result<()> {
        self.begin(Operation::Remove(key.into()))
    }
    pub fn enable(&self, key: &str, enabled: bool) -> Result<()> {
        self.begin(Operation::Enable(key.into(), enabled))
    }
    pub fn cancel(&self) {
        self.0.cancel.store(true, Ordering::Release);
    }
    pub fn search(&self, query: &str, limit: usize) -> Result<SearchReport> {
        search::search(&self.0.root, &self.state(), query, limit)
    }
    pub fn resolve(&self, hit: &DocHit) -> Result<url::Url> {
        search::resolve(&self.0.root, hit)
    }
    pub fn read_page(&self, hit: &DocHit, full_page: bool) -> Result<DocPage> {
        ensure!(
            self.state()
                .installed
                .iter()
                .any(|item| item.key == hit.installed.key
                    && item.directory == hit.installed.directory),
            "文档集已更新或删除，请重新搜索"
        );
        reader::load(&self.0.root, hit, full_page)
    }
    pub fn follow_link(&self, hit: &DocHit, link: &str) -> Result<LinkTarget> {
        reader::follow(&self.0.root, hit, link)
    }
    pub fn page_url(&self, hit: &DocHit) -> Result<url::Url> {
        ensure!(
            self.state()
                .installed
                .iter()
                .any(|item| item.key == hit.installed.key
                    && item.directory == hit.installed.directory),
            "文档集已更新或删除，请重新搜索"
        );
        let file = self.resolve(hit)?;
        let mut server = self
            .0
            .server
            .lock()
            .map_err(|_| anyhow::anyhow!("文档服务不可用"))?;
        let installed = self.state().installed;
        server.retain(|directory, _| installed.iter().any(|item| &item.directory == directory));
        if !server.contains_key(&hit.installed.directory) {
            let documents = hit
                .installed
                .bundle_path(&self.0.root)?
                .join("Contents/Resources/Documents");
            server.insert(
                hit.installed.directory.clone(),
                server::Server::start(&documents)?,
            );
        }
        server.get(&hit.installed.directory).unwrap().page(
            &hit.installed
                .bundle_path(&self.0.root)?
                .join("Contents/Resources/Documents"),
            &file,
        )
    }

    fn load(&self) -> Result<()> {
        fs::create_dir_all(self.0.root.join("sets"))?;
        let installed: Vec<Installed> =
            read_json(&self.0.root.join("installed.json"))?.unwrap_or_default();
        for docset in &installed {
            Available::from_key(docset.key.clone())?;
            // Validate identifiers without requiring every installed document to exist.
            ensure!(
                docset.directory.len() == 65
                    && docset.directory.as_bytes()[32] == b'-'
                    && docset
                        .directory
                        .bytes()
                        .enumerate()
                        .all(|(i, b)| i == 32 || b.is_ascii_hexdigit()),
                "安装列表损坏"
            );
            download::safe_relative(&docset.bundle.to_string_lossy())?;
        }
        let catalog: Option<Vec<Available>> = read_json(&self.0.root.join("catalog.json"))?;
        if let Some(catalog) = &catalog {
            for entry in catalog {
                Available::from_key(entry.key.clone())?;
            }
        }
        self.0.state.send_modify(|state| {
            if let Some(catalog) = catalog.filter(|items| !items.is_empty()) {
                state.catalog = catalog;
            }
            state.installed = installed;
            state.loaded = true;
            state.revision += 1;
        });
        Ok(())
    }
    fn begin(&self, op: Operation) -> Result<()> {
        ensure!(
            matches!(op, Operation::Refresh) || self.state().loaded,
            "文档集列表尚未就绪，请刷新重试"
        );
        ensure!(
            self.0
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "已有文档集任务正在进行"
        );
        self.0.cancel.store(false, Ordering::Release);
        self.0.state.send_modify(|state| {
            state.error = None;
            state.notice = None;
            state.progress = Some(Progress {
                stage: "准备中…".into(),
                ..Default::default()
            });
        });
        let worker = self.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.run(op)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("文档集任务意外中断")));
            worker.0.busy.store(false, Ordering::Release);
            worker.0.state.send_modify(|state| {
                state.progress = None;
                match result {
                    Ok(message) => state.notice = Some(message),
                    Err(_) if worker.0.cancel.load(Ordering::Acquire) => {
                        state.notice = Some("已取消文档集任务".into())
                    }
                    Err(error) => state.error = Some(format!("{error:#}")),
                }
            });
        });
        Ok(())
    }
    fn run(&self, op: Operation) -> Result<String> {
        let mut installed = self.state().installed;
        match op {
            Operation::Refresh => {
                if !self.state().loaded {
                    self.load()?;
                }
                let entries = download::catalog(&self.0.cancel)?;
                write_json(&self.0.root.join("catalog.json"), &entries)?;
                let count = entries.len();
                self.0.state.send_modify(|state| state.catalog = entries);
                return Ok(format!("已刷新 {count} 个 Dash 官方文档集"));
            }
            Operation::Install(key) => {
                let entry = self
                    .state()
                    .catalog
                    .into_iter()
                    .find(|entry| entry.key == key)
                    .context("文档集不在官方列表中")?;
                let previous = installed.iter().find(|docset| docset.key == key).cloned();
                let document = download::install(
                    &self.0.root,
                    &entry,
                    previous.as_ref(),
                    &self.0.cancel,
                    |progress| {
                        self.0
                            .state
                            .send_modify(|state| state.progress = Some(progress));
                    },
                )?;
                let Some(document) = document else {
                    return Ok(format!("{} 已是最新版本", entry.title));
                };
                installed.retain(|docset| docset.key != key);
                installed.push(document.clone());
                if let Err(error) = write_json(&self.0.root.join("installed.json"), &installed) {
                    let _ = remove_owned(&self.0.root, &document.directory);
                    return Err(error);
                }
                self.publish_installed(installed);
                if let Some(old) = previous {
                    let _ = remove_owned(&self.0.root, &old.directory);
                }
                return Ok(format!(
                    "{} {} 安装完成，可输入 h 搜索",
                    document.title, document.version
                ));
            }
            Operation::Remove(key) => {
                let previous = installed
                    .iter()
                    .find(|docset| docset.key == key)
                    .cloned()
                    .context("文档集未安装")?;
                installed.retain(|docset| docset.key != key);
                write_json(&self.0.root.join("installed.json"), &installed)?;
                self.publish_installed(installed);
                remove_owned(&self.0.root, &previous.directory)
                    .context("已移出检索列表，文件暂被占用，稍后可重新安装以清理")?;
                return Ok(format!("已删除 {}", previous.title));
            }
            Operation::Enable(key, enabled) => {
                installed
                    .iter_mut()
                    .find(|docset| docset.key == key)
                    .context("文档集未安装")?
                    .enabled = enabled;
            }
        }
        write_json(&self.0.root.join("installed.json"), &installed)?;
        self.publish_installed(installed);
        Ok("已保存文档检索设置".into())
    }
    fn publish_installed(&self, installed: Vec<Installed>) {
        let directories = installed
            .iter()
            .map(|item| item.directory.clone())
            .collect::<std::collections::HashSet<_>>();
        self.0.state.send_modify(|state| {
            state.installed = installed;
            state.revision += 1;
        });
        if let Ok(mut servers) = self.0.server.lock() {
            servers.retain(|directory, _| directories.contains(directory));
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut bytes = vec![];
    fs::File::open(path)?
        .take(MAX_METADATA + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_METADATA, "文档集列表过大");
    Ok(Some(
        serde_json::from_slice(&bytes).context("文档集列表损坏")?,
    ))
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("保存目录无效")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|error| error.error)
        .context("无法保存文档集列表")?;
    Ok(())
}
fn nonce() -> String {
    format!(
        "{:032x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
fn remove_owned(root: &Path, directory: &str) -> Result<()> {
    ensure!(
        directory.len() == 65
            && directory.as_bytes()[32] == b'-'
            && directory
                .bytes()
                .enumerate()
                .all(|(i, b)| i == 32 || b.is_ascii_hexdigit()),
        "不能删除非托管目录"
    );
    let sets = root.join("sets").canonicalize()?;
    let target = sets.join(directory);
    if !target.exists() {
        return Ok(());
    }
    let resolved = target.canonicalize()?;
    ensure!(
        resolved.parent() == Some(sets.as_path()),
        "不能删除非托管目录"
    );
    fs::remove_dir_all(resolved)?;
    Ok(())
}
