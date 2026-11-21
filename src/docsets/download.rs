use super::*;
use flate2::read::MultiGzDecoder;
use quick_xml::{Reader, events::Event};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;
const MAX_UNPACKED: u64 = 4 * 1024 * 1024 * 1024;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(12))
        .timeout_read(Duration::from_secs(20))
        .timeout_write(Duration::from_secs(20))
        .try_proxy_from_env(true)
        .redirects(5)
        .build()
}
fn checked(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "已取消文档集任务");
    Ok(())
}
fn text(address: &str, limit: u64, cancel: &AtomicBool) -> Result<String> {
    checked(cancel)?;
    let response = agent()
        .get(address)
        .timeout(Duration::from_secs(25))
        .set("User-Agent", "Moin-Docsets/0.2")
        .call()
        .context("无法读取 Dash 官方文档源")?;
    let mut bytes = vec![];
    response
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    checked(cancel)?;
    ensure!(bytes.len() as u64 <= limit, "文档源响应过大");
    String::from_utf8(bytes).context("文档源编码无效")
}
pub(super) fn catalog(cancel: &AtomicBool) -> Result<Vec<Available>> {
    let content = text(
        "https://api.github.com/repos/Kapeli/feeds/contents",
        MAX_METADATA,
        cancel,
    )?;
    let data: serde_json::Value = serde_json::from_str(&content)?;
    let mut entries = vec![];
    for entry in data.as_array().context("官方目录格式无效")? {
        if entry["type"] != "file" {
            continue;
        }
        if let Some(key) = entry["name"]
            .as_str()
            .and_then(|name| name.strip_suffix(".xml"))
        {
            entries.push(Available::from_key(key.into())?);
        }
    }
    ensure!(
        !entries.is_empty() && entries.len() <= 1000,
        "官方目录为空或过大"
    );
    entries.sort_by_key(|entry| entry.title.to_lowercase());
    Ok(entries)
}

struct Feed {
    version: String,
    urls: Vec<String>,
}
fn parse_feed(content: &str) -> Result<Feed> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);
    let mut depth = 0;
    let mut root_seen = false;
    let mut version = String::new();
    let mut urls: Vec<String> = vec![];
    loop {
        match reader.read_event()? {
            Event::Start(tag) if depth == 0 => {
                ensure!(
                    !root_seen && tag.name().as_ref() == "entry",
                    "文档源不是 Dash XML 订阅"
                );
                root_seen = true;
                depth = 1;
            }
            Event::Start(tag) if depth == 1 && matches!(tag.name().as_ref(), "version" | "url") => {
                let raw = reader.read_text(tag.name())?;
                let value = quick_xml::escape::unescape(&raw)?.trim().to_owned();
                if tag.name().as_ref() == "version" {
                    if version.is_empty() {
                        version = value;
                    }
                } else {
                    let mut url = url::Url::parse(&value)?;
                    ensure!(
                        matches!(url.scheme(), "http" | "https")
                            && url.host_str().is_some()
                            && url.username().is_empty()
                            && url.password().is_none(),
                        "订阅包含不支持的下载地址"
                    );
                    url.set_scheme("https")
                        .map_err(|_| anyhow::anyhow!("下载地址无效"))?;
                    urls.push(url.into());
                }
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                ensure!(depth > 0, "文档源 XML 无效");
                depth -= 1;
            }
            Event::Eof => break,
            Event::DocType(_) => anyhow::bail!("文档源不支持外部实体"),
            _ => {}
        }
    }
    ensure!(depth == 0 && root_seen, "文档源 XML 不完整");
    ensure!(
        !version.is_empty() && version.len() <= 100 && !urls.is_empty(),
        "文档源缺少版本或下载地址"
    );
    urls.truncate(8);
    // Prefer the nearby official mirror; preserve every other feed-provided
    // endpoint as a fallback instead of replacing Kapeli's download sources.
    urls.sort_by_key(|address| {
        !url::Url::parse(address)
            .ok()
            .is_some_and(|url| url.host_str() == Some("tokyo.kapeli.com"))
    });
    Ok(Feed { version, urls })
}

pub(super) fn install(
    root: &Path,
    entry: &Available,
    previous: Option<&Installed>,
    cancel: &AtomicBool,
    report: impl Fn(Progress),
) -> Result<Option<Installed>> {
    let report_stage = |stage: &str, received, total| {
        report(Progress {
            title: entry.title.clone(),
            stage: stage.into(),
            received,
            total,
        })
    };
    report_stage("读取官方版本…", 0, None);
    let encoded: String = url::form_urlencoded::byte_serialize(entry.key.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    let xml = text(
        &format!("https://kapeli.com/feeds/{encoded}.xml"),
        2 * 1024 * 1024,
        cancel,
    )
    .or_else(|error| {
        checked(cancel)?;
        text(
            &format!("https://raw.githubusercontent.com/Kapeli/feeds/master/{encoded}.xml"),
            2 * 1024 * 1024,
            cancel,
        )
        .with_context(|| format!("官方订阅读取失败 · {error:#}"))
    })?;
    let feed = parse_feed(&xml)?;
    if let Some(previous) = previous {
        if previous.version == feed.version && previous.bundle_path(root).is_ok() {
            return Ok(None);
        }
    }
    let sets = root.join("sets");
    fs::create_dir_all(&sets)?;
    let sets = sets.canonicalize()?;
    let stage = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(&sets)?;
    let archive = stage.path().join("download.tgz");
    let mut last_error = None;
    for address in &feed.urls {
        checked(cancel)?;
        let mirror = url::Url::parse(address)?
            .host_str()
            .unwrap_or("Kapeli")
            .to_owned();
        let downloaded = (|| {
            let response = agent()
                .get(address)
                .set("User-Agent", "Moin-Docsets/0.2")
                .call()?;
            let total = response
                .header("content-length")
                .and_then(|v| v.parse::<u64>().ok());
            ensure!(
                total.is_none_or(|size| size <= MAX_DOWNLOAD),
                "文档集压缩包超过 512 MiB"
            );
            let mut file = fs::File::create(&archive)?;
            let mut input = response.into_reader();
            let mut received = 0;
            let mut last = Instant::now();
            let mut buffer = [0; 64 * 1024];
            let label = format!("正在下载 · {mirror}");
            report_stage(&label, 0, total);
            loop {
                checked(cancel)?;
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                received += count as u64;
                ensure!(received <= MAX_DOWNLOAD, "文档集压缩包超过 512 MiB");
                file.write_all(&buffer[..count])?;
                if last.elapsed() >= Duration::from_millis(150) {
                    report_stage(&label, received, total);
                    last = Instant::now();
                }
            }
            ensure!(
                total.is_none_or(|size| size == received),
                "下载未完成，请重试"
            );
            report_stage("正在解压并验证…", received, total);
            Ok::<_, anyhow::Error>(())
        })();
        match downloaded {
            Ok(()) => {
                last_error = None;
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    if let Some(error) = last_error {
        return Err(error.context("Dash 官方镜像下载失败"));
    }
    checked(cancel)?;
    let payload = stage.path().join("payload");
    fs::create_dir(&payload)?;
    let (bundle, size) = unpack(&archive, &payload, cancel)?;
    fs::remove_file(&archive)?;
    report_stage("建立文档检索…", size, Some(size));
    let mut document = inspect_bundle(&bundle, entry, &feed.version, size)?;
    document.enabled = previous.is_none_or(|old| old.enabled);
    checked(cancel)?;
    let directory = format!("{}-{}", entry.hash(), nonce());
    document.bundle = PathBuf::from("payload").join(bundle.strip_prefix(&payload)?);
    document.directory = directory.clone();
    let destination = sets.join(directory);
    ensure!(destination.parent() == Some(sets.as_path()), "安装目录无效");
    fs::rename(stage.path(), destination).context("无法发布文档集，新版本未生效")?;
    Ok(Some(document))
}

pub(super) fn safe_relative(value: &str) -> Result<PathBuf> {
    ensure!(
        !value.starts_with(['/', '\\']) && !value.contains([':', '\0']),
        "文档集包含非法路径"
    );
    let mut path = PathBuf::new();
    for part in value.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        ensure!(part != ".." && part.len() <= 255, "文档集包含越界路径");
        #[cfg(windows)]
        {
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            ensure!(
                !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    && !(stem.len() == 4
                        && (stem.starts_with("COM") || stem.starts_with("LPT"))
                        && stem.as_bytes()[3].is_ascii_digit()),
                "文档集文件名不受当前系统支持"
            );
            ensure!(!part.ends_with([' ', '.']), "文档集文件名不受当前系统支持");
        }
        path.push(part);
    }
    ensure!(!path.as_os_str().is_empty(), "文档集路径为空");
    Ok(path)
}

#[derive(Default)]
struct Budget {
    size: u64,
    files: u64,
}
fn extract(archive: &Path, base: &Path, cancel: &AtomicBool, budget: &mut Budget) -> Result<()> {
    let decoder = MultiGzDecoder::new(fs::File::open(archive)?);
    let mut archive = tar::Archive::new(decoder);
    let mut links = vec![];
    for item in archive.entries()? {
        checked(cancel)?;
        let mut item = item?;
        let raw = item.path()?.to_string_lossy().into_owned();
        if raw == "." || raw == "./" {
            continue;
        }
        let relative = safe_relative(&raw)?;
        if relative
            .components()
            .any(|part| part.as_os_str() == "__MACOSX" || part.as_os_str() == ".DS_Store")
        {
            continue;
        }
        budget.files += 1;
        ensure!(budget.files <= 300_000, "文档集文件数量超过上限");
        let destination = base.join(&relative);
        let kind = item.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        if kind.is_symlink() || kind.is_hard_link() {
            let raw_target = item
                .link_name()?
                .context("文档集链接缺少目标")?
                .to_string_lossy()
                .into_owned();
            ensure!(
                !raw_target.starts_with(['/', '\\']) && !raw_target.contains([':', '\0']),
                "文档集链接越界"
            );
            let mut target = if kind.is_symlink() {
                relative.parent().unwrap_or(Path::new("")).to_owned()
            } else {
                PathBuf::new()
            };
            for part in raw_target.split(['/', '\\']) {
                if part == ".." {
                    ensure!(target.pop(), "文档集链接越界");
                } else if !part.is_empty() && part != "." {
                    target.push(part);
                }
            }
            let target = safe_relative(&target.to_string_lossy())?;
            links.push((destination, base.join(target)));
            continue;
        }
        ensure!(kind.is_file(), "文档集包含不支持的特殊文件");
        let size = item.size();
        ensure!(size <= 256 * 1024 * 1024, "文档集单个文件过大");
        budget.size = budget.size.checked_add(size).context("文档集大小无效")?;
        ensure!(budget.size <= MAX_UNPACKED, "文档集解压大小超过 4 GiB");
        fs::create_dir_all(destination.parent().unwrap())?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .context("文档集存在重复或无效文件")?;
        let mut buffer = [0; 64 * 1024];
        let mut written = 0;
        loop {
            checked(cancel)?;
            let n = item.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            written += n as u64;
        }
        ensure!(written == size, "文档集文件不完整");
    }
    for _ in 0..8 {
        if links.is_empty() {
            break;
        }
        let mut remaining = vec![];
        for (destination, target) in links {
            checked(cancel)?;
            if !target.is_file() {
                remaining.push((destination, target));
                continue;
            }
            ensure!(
                target.canonicalize()?.starts_with(base.canonicalize()?),
                "文档集链接越界"
            );
            budget.size += target.metadata()?.len();
            ensure!(budget.size <= MAX_UNPACKED, "文档集解压大小超过 4 GiB");
            fs::create_dir_all(destination.parent().unwrap())?;
            ensure!(!destination.exists(), "文档集存在重复链接");
            fs::copy(target, destination)?;
        }
        links = remaining;
    }
    ensure!(links.is_empty(), "文档集包含无效或不支持的目录链接");
    Ok(())
}
pub(super) fn unpack(
    archive: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<(PathBuf, u64)> {
    let mut budget = Budget::default();
    extract(archive, destination, cancel, &mut budget)?;
    let bundles = walkdir::WalkDir::new(destination)
        .max_depth(4)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_dir()
                && entry.path().extension().is_some_and(|ext| ext == "docset")
                && entry
                    .path()
                    .join("Contents/Resources/docSet.dsidx")
                    .is_file()
        })
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    ensure!(
        bundles.len() == 1,
        "压缩包必须包含一个完整的 .docset 文档集"
    );
    let bundle = bundles.into_iter().next().unwrap();
    let resources = bundle.join("Contents/Resources");
    let nested = resources.join("tarix.tgz");
    if !resources.join("Documents").is_dir() && nested.is_file() {
        let mut probe = tar::Archive::new(MultiGzDecoder::new(fs::File::open(&nested)?));
        let mut first = None;
        for item in probe.entries()? {
            let item = item?;
            let path = item.path()?;
            if path.as_os_str() == "." || path.as_os_str() == "./" {
                continue;
            }
            first = Some(safe_relative(&path.to_string_lossy())?);
            break;
        }
        let first = first.context("嵌套文档压缩包为空")?;
        let base = if first.starts_with("Contents") {
            bundle.clone()
        } else if first.starts_with("Documents") {
            resources.clone()
        } else if first
            .components()
            .next()
            .is_some_and(|part| part.as_os_str() == bundle.file_name().unwrap())
        {
            bundle.parent().unwrap().to_owned()
        } else {
            resources.join("Documents")
        };
        fs::create_dir_all(&base)?;
        extract(&nested, &base, cancel, &mut budget)?;
        fs::remove_file(nested)?;
    }
    ensure!(
        resources.join("Documents").is_dir(),
        "文档集没有可用的 HTML 文档目录"
    );
    Ok((bundle, budget.size))
}

pub(super) fn inspect_bundle(
    bundle: &Path,
    entry: &Available,
    version: &str,
    size: u64,
) -> Result<Installed> {
    let info = bundle.join("Contents/Info.plist");
    let info = if info.exists() {
        info
    } else {
        bundle.join("Contents/info.plist")
    };
    ensure!(info.metadata()?.len() <= MAX_METADATA, "文档集元数据过大");
    let plist = plist::Value::from_file(info).context("无法解析 docset 的 Info.plist")?;
    let values = plist.as_dictionary().context("docset 元数据格式无效")?;
    let mut keywords = HashSet::from([entry.key.to_lowercase(), entry.title.to_lowercase()]);
    for key in [
        "DashDocSetKeyword",
        "DocSetPlatformFamily",
        "DashDocSetPluginKeyword",
    ] {
        if let Some(word) = values
            .get(key)
            .and_then(plist::Value::as_string)
            .filter(|word| !word.is_empty() && word.len() <= 100)
        {
            keywords.insert(word.to_lowercase());
        }
    }
    let mut keywords = keywords.into_iter().collect::<Vec<_>>();
    keywords.sort();
    let homepage = values
        .get("dashIndexFilePath")
        .and_then(plist::Value::as_string)
        .map(str::to_owned)
        .or_else(|| {
            bundle
                .join("Contents/Resources/Documents/index.html")
                .is_file()
                .then(|| "index.html".into())
        });
    let symbols = super::search::validate_index(&bundle.join("Contents/Resources/docSet.dsidx"))?;
    Ok(Installed {
        key: entry.key.clone(),
        title: entry.title.clone(),
        version: version.into(),
        enabled: true,
        keywords,
        symbols,
        size,
        directory: String::new(),
        bundle: PathBuf::new(),
        homepage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(data.len() as u64);
            header.set_cksum();
            builder.append_data(&mut header, *name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }
    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }
    #[test]
    fn official_feed_reads_only_current_version_and_upgrades_mirror_transport() {
        let feed=parse_feed("<entry><version>2.0</version><url>http://tokyo.kapeli.com/feeds/C%2B%2B.tgz?a=1&amp;b=2</url><other-versions><version><name>1.0</name></version></other-versions></entry>").unwrap();
        assert_eq!(feed.version, "2.0");
        assert_eq!(
            feed.urls,
            ["https://tokyo.kapeli.com/feeds/C%2B%2B.tgz?a=1&b=2"]
        );
        for invalid in [
            "<html>error</html>",
            "<entry><version>1</version>",
            "<!DOCTYPE entry SYSTEM 'file:///outside'><entry/>",
        ] {
            assert!(parse_feed(invalid).is_err());
        }
    }
    #[test]
    fn extraction_supports_multimember_gzip_and_nested_tarix_documents() {
        let nested = gzip(&tar(&[(
            "Contents/Resources/Documents/index.html",
            b"offline",
        )]));
        let outer = tar(&[
            ("Fixture.docset/Contents/Resources/docSet.dsidx", b"index"),
            ("Fixture.docset/Contents/Resources/tarix.tgz", &nested),
        ]);
        let split = outer.len() / 2;
        let mut archive = gzip(&outer[..split]);
        archive.extend(gzip(&outer[split..]));
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("fixture.tgz");
        fs::write(&path, archive).unwrap();
        let destination = home.path().join("unpacked");
        fs::create_dir(&destination).unwrap();
        let (bundle, _) = unpack(&path, &destination, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            fs::read(bundle.join("Contents/Resources/Documents/index.html")).unwrap(),
            b"offline"
        );
        assert!(!bundle.join("Contents/Resources/tarix.tgz").exists());
    }
    #[test]
    fn extraction_rejects_traversal_and_honors_cancellation() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("bad.tgz");
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(1);
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..9].copy_from_slice(b"../escape");
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append(&header, &b"x"[..]).unwrap();
        fs::write(&path, gzip(&builder.into_inner().unwrap())).unwrap();
        let destination = home.path().join("owned");
        fs::create_dir(&destination).unwrap();
        assert!(
            extract(
                &path,
                &destination,
                &AtomicBool::new(false),
                &mut Budget::default()
            )
            .is_err()
        );
        assert!(!home.path().join("escape").exists());
        assert!(
            extract(
                &path,
                &destination,
                &AtomicBool::new(true),
                &mut Budget::default()
            )
            .is_err()
        );
        for value in [
            "../outside",
            "C:\\outside",
            "/outside",
            "a/../../outside",
            "a:stream",
        ] {
            assert!(safe_relative(value).is_err());
        }
    }
}
