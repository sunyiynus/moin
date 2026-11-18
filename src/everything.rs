//! Windows filename search backed by the Everything SDK.
//!
//! The SDK DLL is embedded in Moin's executable and extracted to the local data
//! directory on first use. Everything itself is a separate application: Moin
//! only talks to it over local IPC and falls back to its own index when it is
//! not running.

use anyhow::{Result, bail};
use moin::index::{self, Hit, HitKind, SearchMode};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Checking,
    Ready { version: String },
    Starting { version: Option<String> },
    NotRunning { executable: Option<PathBuf> },
    Unavailable(String),
    Unsupported,
}

impl Default for Status {
    fn default() -> Self {
        Self::Checking
    }
}

impl Status {
    pub fn executable(&self) -> Option<&Path> {
        match self {
            Self::NotRunning { executable } => executable.as_deref(),
            _ => None,
        }
    }
}

/// Query the filename index, preferring Everything when it is available.
pub fn search_with_index(
    database: &Path,
    data_dir: &Path,
    query: &str,
    mode: SearchMode,
    limit: usize,
    roots: &[PathBuf],
) -> Result<Vec<Hit>> {
    if limit == 0 || !matches!(mode, SearchMode::All | SearchMode::Files) || query.trim().is_empty()
    {
        return index::search(database, query, mode, limit);
    }

    let overfetch = limit.saturating_mul(2).max(limit);
    let mut names = index::search_names(database, query, mode, overfetch)?;
    match search(data_dir, query, limit, roots) {
        Ok(external) => names = merge_names(names, external, query, limit),
        Err(_) => names.truncate(limit),
    }

    if mode == SearchMode::All && names.len() < limit {
        let remaining = limit - names.len();
        match index::retrieve(database, query, remaining) {
            Ok(text) => append_text(&mut names, text, limit),
            Err(error) if names.is_empty() => return Err(error),
            Err(_) => {}
        }
    }
    Ok(names)
}

/// Merge local and Everything filename results, preferring local metadata for
/// duplicate paths and ranking exact/prefix matches first.
pub fn merge_names(internal: Vec<Hit>, external: Vec<Hit>, query: &str, limit: usize) -> Vec<Hit> {
    let query = query.trim().to_lowercase();
    let mut seen = HashSet::new();
    let mut merged = Vec::with_capacity(internal.len() + external.len());
    for hit in internal.into_iter().chain(external) {
        if seen.insert(path_key(&hit.path)) {
            merged.push(hit);
        }
    }
    merged.sort_by_key(|hit| rank(hit, &query));
    merged.truncate(limit);
    merged
}

fn append_text(names: &mut Vec<Hit>, text: Vec<Hit>, limit: usize) {
    let mut seen = names
        .iter()
        .map(|hit| path_key(&hit.path))
        .collect::<HashSet<_>>();
    for hit in text {
        if names.len() >= limit {
            break;
        }
        if seen.insert(path_key(&hit.path)) {
            names.push(hit);
        }
    }
}

fn rank(hit: &Hit, query: &str) -> (u8, u8, usize, String) {
    let title = hit.title.to_lowercase();
    let score = if query.is_empty() || title == query {
        0
    } else if title.starts_with(query) {
        1
    } else if title.contains(query) {
        2
    } else {
        3
    };
    let kind = match hit.kind {
        HitKind::App => 0,
        HitKind::File => 1,
        HitKind::Text => 2,
    };
    (kind, score, title.chars().count(), title)
}

fn path_key(path: &Path) -> String {
    let mut key = path.to_string_lossy().replace('/', "\\");
    if let Some(rest) = key.strip_prefix("\\\\?\\UNC\\") {
        key = format!("\\\\{rest}");
    } else if let Some(rest) = key.strip_prefix("\\\\?\\") {
        key = rest.to_owned();
    }
    key.to_lowercase()
}

fn in_roots(path: &Path, roots: &[PathBuf], data_dir: &Path) -> bool {
    let key = path_key(path);
    let data_dir = path_key(data_dir);
    if !data_dir.is_empty() && (key == data_dir || key.starts_with(&format!("{data_dir}\\"))) {
        return false;
    }
    roots.iter().any(|root| {
        let root = path_key(root);
        let root = root.trim_end_matches('\\');
        !root.is_empty() && (key == root || key.starts_with(&format!("{root}\\")))
    })
}

fn escaped_query(query: &str) -> String {
    format!("\"{}\"", query.trim().replace('"', " "))
}

pub fn status(data_dir: &Path) -> Status {
    platform::status(data_dir)
}

pub fn search(data_dir: &Path, query: &str, limit: usize, roots: &[PathBuf]) -> Result<Vec<Hit>> {
    platform::search(data_dir, query, limit, roots)
}

pub fn start() -> Result<PathBuf> {
    platform::start()
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod platform {
    use super::*;
    use anyhow::Context;
    use std::{
        fs,
        os::windows::ffi::{OsStrExt, OsStringExt},
        sync::{Mutex, OnceLock},
    };
    use windows::{
        Win32::{
            Foundation::{FreeLibrary, HMODULE},
            System::LibraryLoader::{GetProcAddress, LoadLibraryW},
        },
        core::{PCSTR, PCWSTR},
    };

    const DLL_BYTES: &[u8] = include_bytes!("../assets/everything/Everything64.dll");
    const DLL_FILE: &str = "Everything64-sdk2.dll";
    const ERROR_IPC: u32 = 2;
    const REQUEST_FILE_NAME: u32 = 1;
    const REQUEST_PATH: u32 = 2;
    const SORT_NAME_ASCENDING: u32 = 1;

    type SetSearchW = unsafe extern "system" fn(*const u16);
    type SetMatchPath = unsafe extern "system" fn(i32);
    type SetMatchCase = unsafe extern "system" fn(i32);
    type SetRegex = unsafe extern "system" fn(i32);
    type SetMax = unsafe extern "system" fn(u32);
    type SetSort = unsafe extern "system" fn(u32);
    type SetRequestFlags = unsafe extern "system" fn(u32);
    type QueryW = unsafe extern "system" fn(i32) -> i32;
    type GetNumResults = unsafe extern "system" fn() -> u32;
    type IsFileResult = unsafe extern "system" fn(u32) -> i32;
    type IsDbLoaded = unsafe extern "system" fn() -> i32;
    type GetResultFullPathNameW = unsafe extern "system" fn(u32, *mut u16, u32) -> u32;
    type GetLastError = unsafe extern "system" fn() -> u32;
    type GetMajorVersion = unsafe extern "system" fn() -> u32;
    type GetMinorVersion = unsafe extern "system" fn() -> u32;
    type GetRevision = unsafe extern "system" fn() -> u32;
    type GetBuildNumber = unsafe extern "system" fn() -> u32;
    type CleanUp = unsafe extern "system" fn();

    #[derive(Clone, Copy)]
    struct Api {
        set_search: SetSearchW,
        set_match_path: SetMatchPath,
        set_match_case: SetMatchCase,
        set_regex: SetRegex,
        set_max: SetMax,
        set_sort: SetSort,
        set_request_flags: SetRequestFlags,
        query: QueryW,
        get_num_results: GetNumResults,
        is_file_result: IsFileResult,
        is_db_loaded: IsDbLoaded,
        get_result_full_path: GetResultFullPathNameW,
        get_last_error: GetLastError,
        major_version: GetMajorVersion,
        minor_version: GetMinorVersion,
        revision: GetRevision,
        build_number: GetBuildNumber,
        cleanup: CleanUp,
    }

    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    static QUERY_LOCK: Mutex<()> = Mutex::new(());

    pub(super) fn status(data_dir: &Path) -> Status {
        let api = match api(data_dir) {
            Ok(api) => api,
            Err(reason) => return Status::Unavailable(reason),
        };
        let _guard = QUERY_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let loaded = (api.is_db_loaded)() != 0;
            let error = (api.get_last_error)();
            let version = version(api);
            if loaded {
                return Status::Ready {
                    version: version.unwrap_or_else(|| "未知版本".into()),
                };
            }
            if error == ERROR_IPC {
                return Status::NotRunning {
                    executable: find_executable(),
                };
            }
            if error != 0 {
                return Status::Unavailable(format!("Everything 错误码 {error}"));
            }
            if version.is_none() {
                return Status::NotRunning {
                    executable: find_executable(),
                };
            }
            Status::Starting { version }
        }
    }

    pub(super) fn search(
        data_dir: &Path,
        query: &str,
        limit: usize,
        roots: &[PathBuf],
    ) -> Result<Vec<Hit>> {
        if limit == 0 || query.trim().is_empty() || roots.is_empty() {
            return Ok(Vec::new());
        }
        let api = api(data_dir).map_err(anyhow::Error::msg)?;
        let _guard = QUERY_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let search = escaped_query(query);
        let wide: Vec<u16> = search.encode_utf16().chain(std::iter::once(0)).collect();
        let max = limit.saturating_mul(4).clamp(limit, 1000) as u32;
        unsafe {
            (api.set_search)(wide.as_ptr());
            (api.set_match_path)(if query.contains(['/', '\\', ':']) {
                1
            } else {
                0
            });
            (api.set_match_case)(0);
            (api.set_regex)(0);
            (api.set_request_flags)(REQUEST_FILE_NAME | REQUEST_PATH);
            (api.set_sort)(SORT_NAME_ASCENDING);
            (api.set_max)(max);
            if (api.query)(1) == 0 {
                let error = (api.get_last_error)();
                (api.cleanup)();
                bail!("Everything 查询失败（错误码 {error}）");
            }
            let count = (api.get_num_results)();
            let mut hits = Vec::new();
            for index in 0..count {
                if (api.is_file_result)(index) == 0 {
                    continue;
                }
                let Some(path) = result_path(api, index) else {
                    continue;
                };
                if !in_roots(&path, roots, data_dir) {
                    continue;
                }
                let title = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let detail = path
                    .parent()
                    .map(|parent| parent.display().to_string())
                    .unwrap_or_default();
                hits.push(Hit {
                    title,
                    path,
                    detail,
                    excerpt: String::new(),
                    kind: HitKind::File,
                    chunk: None,
                });
            }
            (api.cleanup)();
            Ok(hits)
        }
    }

    pub(super) fn start() -> Result<PathBuf> {
        let executable = find_executable().context("未找到 Everything.exe，请先安装 Everything")?;
        std::process::Command::new(&executable)
            .arg("-startup")
            .spawn()
            .with_context(|| format!("无法启动 {}", executable.display()))?;
        Ok(executable)
    }

    fn api(data_dir: &Path) -> Result<&'static Api, String> {
        match API.get_or_init(|| load(data_dir)) {
            Ok(api) => Ok(api),
            Err(error) => Err(error.clone()),
        }
    }

    fn load(data_dir: &Path) -> Result<Api, String> {
        let path = ensure_dll(data_dir)?;
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
            .map_err(|error| format!("无法加载 Everything SDK：{error}"))?;
        match unsafe { Api::load(module) } {
            Ok(api) => Ok(api),
            Err(error) => {
                unsafe {
                    let _ = FreeLibrary(module);
                }
                Err(error)
            }
        }
    }

    impl Api {
        unsafe fn load(module: HMODULE) -> Result<Self, String> {
            Ok(Self {
                set_search: unsafe { symbol(module, b"Everything_SetSearchW\0") }?,
                set_match_path: unsafe { symbol(module, b"Everything_SetMatchPath\0") }?,
                set_match_case: unsafe { symbol(module, b"Everything_SetMatchCase\0") }?,
                set_regex: unsafe { symbol(module, b"Everything_SetRegex\0") }?,
                set_max: unsafe { symbol(module, b"Everything_SetMax\0") }?,
                set_sort: unsafe { symbol(module, b"Everything_SetSort\0") }?,
                set_request_flags: unsafe { symbol(module, b"Everything_SetRequestFlags\0") }?,
                query: unsafe { symbol(module, b"Everything_QueryW\0") }?,
                get_num_results: unsafe { symbol(module, b"Everything_GetNumResults\0") }?,
                is_file_result: unsafe { symbol(module, b"Everything_IsFileResult\0") }?,
                is_db_loaded: unsafe { symbol(module, b"Everything_IsDBLoaded\0") }?,
                get_result_full_path: unsafe {
                    symbol(module, b"Everything_GetResultFullPathNameW\0")
                }?,
                get_last_error: unsafe { symbol(module, b"Everything_GetLastError\0") }?,
                major_version: unsafe { symbol(module, b"Everything_GetMajorVersion\0") }?,
                minor_version: unsafe { symbol(module, b"Everything_GetMinorVersion\0") }?,
                revision: unsafe { symbol(module, b"Everything_GetRevision\0") }?,
                build_number: unsafe { symbol(module, b"Everything_GetBuildNumber\0") }?,
                cleanup: unsafe { symbol(module, b"Everything_CleanUp\0") }?,
            })
        }
    }

    unsafe fn symbol<F: Copy>(module: HMODULE, name: &'static [u8]) -> Result<F, String> {
        let Some(procedure) = (unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }) else {
            let name = String::from_utf8_lossy(&name[..name.len().saturating_sub(1)]);
            return Err(format!("Everything SDK 缺少导出 {name}"));
        };
        Ok(unsafe { std::mem::transmute_copy(&procedure) })
    }

    fn ensure_dll(data_dir: &Path) -> Result<PathBuf, String> {
        let directory = data_dir.join("bin");
        fs::create_dir_all(&directory).map_err(|error| format!("无法创建 SDK 目录：{error}"))?;
        let path = directory.join(DLL_FILE);
        if fs::read(&path).ok().as_deref() != Some(DLL_BYTES) {
            let temporary = directory.join(format!("{DLL_FILE}.tmp"));
            fs::write(&temporary, DLL_BYTES)
                .map_err(|error| format!("无法写入 Everything SDK：{error}"))?;
            if let Err(error) = fs::rename(&temporary, &path) {
                let _ = fs::remove_file(&temporary);
                if fs::read(&path).ok().as_deref() != Some(DLL_BYTES) {
                    return Err(format!("无法写入 Everything SDK：{error}"));
                }
            }
        }
        Ok(std::path::absolute(&path).unwrap_or(path))
    }

    unsafe fn version(api: &Api) -> Option<String> {
        let major = unsafe { (api.major_version)() };
        if major == 0 {
            return None;
        }
        Some(format!(
            "{major}.{}.{}.{}",
            unsafe { (api.minor_version)() },
            unsafe { (api.revision)() },
            unsafe { (api.build_number)() }
        ))
    }

    unsafe fn result_path(api: &Api, index: u32) -> Option<PathBuf> {
        let mut size = 32_768usize;
        loop {
            let mut buffer = vec![0u16; size];
            let length =
                unsafe { (api.get_result_full_path)(index, buffer.as_mut_ptr(), size as u32) };
            if length == 0 {
                return None;
            }
            if (length as usize) + 1 < size {
                buffer.truncate(length as usize);
                return Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer)));
            }
            size = size.saturating_mul(2);
            if size > 131_072 {
                return None;
            }
        }
    }

    fn find_executable() -> Option<PathBuf> {
        let mut candidates = Vec::new();
        for key in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(key) {
                candidates.push(
                    PathBuf::from(base)
                        .join("Everything")
                        .join("Everything.exe"),
                );
            }
        }
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(base)
                    .join("Programs")
                    .join("Everything")
                    .join("Everything.exe"),
            );
        }
        for path in [
            "C:\\Program Files\\Everything\\Everything.exe",
            "C:\\Program Files (x86)\\Everything\\Everything.exe",
        ] {
            candidates.push(PathBuf::from(path));
        }
        if let Some(paths) = std::env::var_os("PATH") {
            candidates
                .extend(std::env::split_paths(&paths).map(|path| path.join("Everything.exe")));
        }
        candidates.into_iter().find(|path| path.is_file())
    }
}

#[cfg(all(windows, not(target_arch = "x86_64")))]
mod platform {
    use super::*;

    pub(super) fn status(_: &Path) -> Status {
        Status::Unavailable("Everything SDK 目前仅支持 64 位 Windows".into())
    }

    pub(super) fn search(_: &Path, _: &str, _: usize, _: &[PathBuf]) -> Result<Vec<Hit>> {
        bail!("Everything 文件搜索目前仅支持 64 位 Windows")
    }

    pub(super) fn start() -> Result<PathBuf> {
        bail!("Everything 文件搜索目前仅支持 64 位 Windows")
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) fn status(_: &Path) -> Status {
        Status::Unsupported
    }

    pub(super) fn search(_: &Path, _: &str, _: usize, _: &[PathBuf]) -> Result<Vec<Hit>> {
        bail!("Everything 文件搜索仅支持 Windows")
    }

    pub(super) fn start() -> Result<PathBuf> {
        bail!("Everything 文件搜索仅支持 Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str, path: &str, kind: HitKind) -> Hit {
        Hit {
            title: title.into(),
            path: PathBuf::from(path),
            detail: String::new(),
            excerpt: String::new(),
            kind,
            chunk: None,
        }
    }

    #[test]
    fn merges_and_deduplicates_by_path() {
        let local = vec![
            hit("Moin", "C:\\Tools\\Moin.exe", HitKind::File),
            hit("Other", "C:\\Other\\file.txt", HitKind::File),
        ];
        let external = vec![
            hit("Moin", "c:/tools/MOIN.exe", HitKind::File),
            hit("Moin", "C:\\Tools\\Moin.exe", HitKind::File),
            hit("Moin", "C:\\Tools\\Moin.exe", HitKind::File),
        ];
        let merged = merge_names(local, external, "moin", 10);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].path, PathBuf::from("C:\\Tools\\Moin.exe"));
    }

    #[test]
    fn ranks_exact_and_prefix_matches_first() {
        let hits = vec![
            hit("moin-extra", "C:\\a\\moin-extra.txt", HitKind::File),
            hit("moin", "C:\\a\\moin.txt", HitKind::File),
            hit("xmoin", "C:\\a\\xmoin.txt", HitKind::File),
        ];
        let merged = merge_names(Vec::new(), hits, "Moin", 3);
        assert_eq!(merged[0].title, "moin");
        assert_eq!(merged[1].title, "moin-extra");
        assert_eq!(merged[2].title, "xmoin");
    }

    #[test]
    fn filters_paths_to_configured_roots() {
        let roots = vec![PathBuf::from("C:\\Users\\me\\Documents")];
        assert!(in_roots(
            Path::new("C:\\Users\\me\\Documents\\note.txt"),
            &roots,
            Path::new("C:\\Users\\me\\AppData\\Moin")
        ));
        assert!(in_roots(
            Path::new("\\\\?\\C:\\Users\\me\\Documents\\note.txt"),
            &roots,
            Path::new("C:\\Users\\me\\AppData\\Moin")
        ));
        assert!(!in_roots(
            Path::new("C:\\Users\\me\\Documents2\\note.txt"),
            &roots,
            Path::new("C:\\Users\\me\\AppData\\Moin")
        ));
        assert!(!in_roots(
            Path::new("C:\\Users\\me\\AppData\\Moin\\index.sqlite3"),
            &roots,
            Path::new("C:\\Users\\me\\AppData\\Moin")
        ));
    }

    #[test]
    fn quotes_literal_queries() {
        assert_eq!(escaped_query("  hello \"world\" "), "\"hello  world \"");
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    #[ignore = "requires Everything to be installed and running"]
    fn combines_local_index_with_everything_results() {
        let data_dir = tempfile::tempdir().unwrap();
        let hits = search_with_index(
            &data_dir.path().join("index.sqlite3"),
            data_dir.path(),
            "Everything.exe",
            SearchMode::Files,
            10,
            &[PathBuf::from("C:\\Program Files\\Everything")],
        )
        .unwrap();
        assert!(
            hits.iter()
                .any(|hit| hit.title.eq_ignore_ascii_case("Everything.exe")),
            "no combined Everything.exe result: {hits:?}"
        );
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    #[ignore = "requires Everything to be installed and running"]
    fn talks_to_a_running_everything_instance() {
        let data_dir = tempfile::tempdir().unwrap();
        let status = status(data_dir.path());
        assert!(
            matches!(status, Status::Ready { .. } | Status::Starting { .. }),
            "unexpected Everything status: {status:?}"
        );
        let hits = search(
            data_dir.path(),
            "Everything.exe",
            10,
            &[PathBuf::from("C:\\")],
        )
        .unwrap();
        assert!(
            hits.iter()
                .any(|hit| hit.title.eq_ignore_ascii_case("Everything.exe")),
            "no Everything.exe result: {hits:?}"
        );
    }
}
