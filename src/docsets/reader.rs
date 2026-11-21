//! A bounded, static reading projection for Kit TextView. Loading and HTML
//! preparation run on a worker; no JavaScript or vendor styling enters the UI.
use super::*;
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::{collections::HashMap, rc::Rc};

const MAX_INPUT: u64 = 8 * 1024 * 1024;
const MAX_TEXT: usize = 96_000;

#[derive(Clone, Debug)]
pub struct DocPage {
    pub hit: DocHit,
    pub html: String,
    pub text: String,
    pub scoped: bool,
    pub full_page: bool,
    pub notice: Option<String>,
    pub images: HashMap<String, PathBuf>,
}
#[derive(Clone, Debug)]
pub enum LinkTarget {
    Local(DocHit),
    External(url::Url),
}

pub(super) fn clean_location(value: &str) -> String {
    let mut result = value.to_owned();
    while let Some(start) = result.find("<dash_entry_") {
        let Some(end) = result[start..].find('>') else {
            result.truncate(start);
            break;
        };
        result.replace_range(start..start + end + 1, "");
    }
    result
}

pub(super) fn follow(root: &Path, hit: &DocHit, link: &str) -> Result<LinkTarget> {
    let base = super::search::resolve(root, hit)?;
    let link = clean_location(link);
    let documents = hit
        .installed
        .bundle_path(root)?
        .join("Contents/Resources/Documents")
        .canonicalize()?;
    let target = resolve_url(&base, &documents, &link)?;
    if matches!(target.scheme(), "http" | "https") {
        ensure!(
            target.host_str().is_some()
                && target.username().is_empty()
                && target.password().is_none(),
            "文档链接无效"
        );
        return Ok(LinkTarget::External(target));
    }
    ensure!(target.scheme() == "file", "此链接类型不受支持");
    let path = local_file(&target, &documents)?;
    // Preserve URL escaping for filenames containing literal %, # or ?.
    let canonical = url::Url::from_file_path(&path).map_err(|_| anyhow::anyhow!("文档链接无效"))?;
    let base =
        url::Url::from_directory_path(&documents).map_err(|_| anyhow::anyhow!("文档目录无效"))?;
    let relative = canonical
        .path()
        .strip_prefix(base.path())
        .context("链接超出了当前文档集")?
        .to_owned();
    let mut next = hit.clone();
    next.path = relative;
    if let Some(query) = target.query() {
        next.path.push('?');
        next.path.push_str(query);
    }
    if let Some(anchor) = target.fragment() {
        next.path.push('#');
        next.path.push_str(anchor);
    }
    next.title = target
        .fragment()
        .filter(|anchor| !anchor.is_empty())
        .map(|anchor| {
            percent_encoding::percent_decode_str(anchor)
                .decode_utf8_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
    Ok(LinkTarget::Local(next))
}

pub(super) fn load(root: &Path, hit: &DocHit, full_page: bool) -> Result<DocPage> {
    let source = super::search::resolve(root, hit)?;
    let path = source
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("文档路径无效"))?;
    let mut bytes = Vec::new();
    fs::File::open(&path)?
        .take(MAX_INPUT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_INPUT,
        "文档页超过 8 MiB，请使用浏览器打开完整页面"
    );
    let html = String::from_utf8(bytes).context("文档页不是 UTF-8 文本")?;
    let documents = hit
        .installed
        .bundle_path(root)?
        .join("Contents/Resources/Documents")
        .canonicalize()?;
    project(hit, &html, &source, &documents, full_page)
}

fn tag(node: &Handle) -> &str {
    match &node.data {
        NodeData::Element { name, .. } => name.local.as_ref(),
        _ => "",
    }
}
fn attr(node: &Handle, key: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| a.name.local.as_ref() == key)
            .map(|a| a.value.to_string()),
        _ => None,
    }
}
fn parent(node: &Handle) -> Option<Handle> {
    let value = node.parent.take();
    let result = value.as_ref().and_then(|parent| parent.upgrade());
    node.parent.set(value);
    result
}
fn find(node: &Handle, predicate: impl Fn(&Handle) -> bool) -> Option<Handle> {
    let mut queue = vec![node.clone()];
    let mut count = 0;
    while let Some(node) = queue.pop() {
        count += 1;
        if count > 200_000 {
            return None;
        }
        if predicate(&node) {
            return Some(node);
        }
        queue.extend(node.children.borrow().iter().rev().cloned());
    }
    None
}
fn heading(node: &Handle) -> Option<u8> {
    match tag(node) {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}
fn siblings_from(node: &Handle, definition: bool) -> Vec<Handle> {
    let Some(parent) = parent(node) else {
        return vec![node.clone()];
    };
    let children = parent.children.borrow();
    let Some(index) = children.iter().position(|child| Rc::ptr_eq(child, node)) else {
        return vec![node.clone()];
    };
    let mut level = heading(node);
    let mut result = vec![];
    for (offset, sibling) in children[index..].iter().enumerate() {
        if offset > 0 {
            if definition && tag(sibling) == "dt" {
                break;
            }
            if let Some(next) = heading(sibling) {
                if level.is_some_and(|level| next <= level) {
                    break;
                }
                level = level.or(Some(next));
            }
            if !definition && tag(sibling) == "a" && attr(sibling, "name").is_some() {
                break;
            }
        }
        result.push(sibling.clone());
    }
    result
}
fn function_nodes(target: &Handle, main: &Handle) -> Vec<Handle> {
    let mut ancestors = vec![];
    let mut next = Some(target.clone());
    while let Some(node) = next {
        if Rc::ptr_eq(&node, main) {
            break;
        }
        next = parent(&node);
        ancestors.push(node);
        if ancestors.len() > 128 {
            break;
        }
    }
    if let Some(details) = ancestors.iter().find(|node| tag(node) == "details") {
        return vec![details.clone()];
    }
    if let Some(dt) = ancestors.iter().find(|node| tag(node) == "dt") {
        return siblings_from(dt, true);
    }
    if let Some(item) = ancestors.iter().find(|node| {
        attr(node, "class")
            .is_some_and(|value| value.split_whitespace().any(|value| value == "memitem"))
    }) {
        return vec![item.clone()];
    }
    for node in &ancestors {
        if heading(node).is_some() {
            return siblings_from(node, false);
        }
        if tag(node) == "section" {
            return vec![node.clone()];
        }
    }
    if let Some(container) = ancestors
        .iter()
        .find(|node| matches!(tag(node), "p" | "div") && !Rc::ptr_eq(node, main))
    {
        return siblings_from(container, false);
    }
    siblings_from(target, false)
}

fn project(
    hit: &DocHit,
    html: &str,
    source: &url::Url,
    documents: &Path,
    full_page: bool,
) -> Result<DocPage> {
    let dom = parse_document(RcDom::default(), Default::default()).one(html);
    let main = find(&dom.document, |node| {
        tag(node) == "main"
            || attr(node, "role").as_deref() == Some("main")
            || attr(node, "id").as_deref() == Some("main-content")
    })
    .or_else(|| find(&dom.document, |node| tag(node) == "article"))
    .or_else(|| find(&dom.document, |node| tag(node) == "body"))
    .unwrap_or_else(|| dom.document.clone());
    let fragment = source.fragment().filter(|value| !value.is_empty());
    let target = fragment.and_then(|raw| {
        let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
        find(&main, |node| {
            ["id", "name"]
                .iter()
                .any(|key| attr(node, key).is_some_and(|value| value == raw || value == decoded))
        })
    });
    let scoped = !full_page && target.as_ref().is_some_and(|node| !Rc::ptr_eq(node, &main));
    let nodes = if scoped {
        function_nodes(target.as_ref().unwrap(), &main)
    } else {
        vec![main]
    };
    let mut writer = Writer {
        html: String::new(),
        text: String::new(),
        images: HashMap::new(),
        source,
        documents,
        remaining: MAX_TEXT,
        truncated: false,
        nodes: 0,
    };
    for node in nodes {
        writer.node(&node, 0);
    }
    ensure!(
        !writer.text.trim().is_empty(),
        "此页面没有可供 TextView 显示的静态正文"
    );
    let mut notices = vec![];
    if fragment.is_some() && target.is_none() && !full_page {
        notices.push("未找到该函数的锚点，已显示页面正文");
    }
    if writer.truncated {
        notices.push("正文较长，已截取前 96,000 个字符；可用浏览器查看完整内容");
    }
    Ok(DocPage {
        hit: hit.clone(),
        html: writer.html,
        text: writer.text,
        scoped,
        full_page,
        notice: (!notices.is_empty()).then(|| notices.join(" · ")),
        images: writer.images,
    })
}

struct Writer<'a> {
    html: String,
    text: String,
    images: HashMap<String, PathBuf>,
    source: &'a url::Url,
    documents: &'a Path,
    remaining: usize,
    truncated: bool,
    nodes: usize,
}
impl Writer<'_> {
    fn node(&mut self, node: &Handle, depth: usize) {
        if self.remaining == 0 || depth > 128 || self.nodes > 100_000 {
            self.truncated = true;
            return;
        }
        self.nodes += 1;
        match &node.data {
            NodeData::Text { contents } => {
                let value = contents.borrow();
                let selected = value.chars().take(self.remaining).collect::<String>();
                let count = selected.chars().count();
                if count < value.chars().count() {
                    self.truncated = true;
                }
                self.remaining -= count;
                self.html.push_str(&escape(&selected));
                self.text.push_str(&selected);
            }
            NodeData::Element { .. } => {
                let name = tag(node);
                if matches!(
                    name,
                    "head"
                        | "script"
                        | "style"
                        | "nav"
                        | "aside"
                        | "footer"
                        | "form"
                        | "input"
                        | "button"
                        | "svg"
                        | "canvas"
                        | "template"
                        | "noscript"
                        | "iframe"
                        | "object"
                        | "wbr"
                ) || attr(node, "hidden").is_some()
                    || attr(node, "aria-hidden").as_deref() == Some("true")
                    || attr(node, "role").is_some_and(|role| {
                        matches!(role.as_str(), "navigation" | "search" | "banner")
                    })
                {
                    return;
                }
                if name == "img" {
                    self.image(node);
                    return;
                }
                let output = match name {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "ul" | "ol" | "li" | "pre"
                    | "code" | "blockquote" | "a" | "table" | "thead" | "tbody" | "tfoot"
                    | "tr" | "th" | "td" | "strong" | "b" | "em" | "i" | "u" | "s" | "del"
                    | "br" | "hr" | "sup" | "sub" | "span" => name,
                    "dt" => "h4",
                    "kbd" | "samp" | "var" => "code",
                    "html" | "body" | "div" | "main" | "article" | "section" | "details"
                    | "summary" | "header" | "dl" | "dd" | "address" | "figure" | "figcaption"
                    | "fieldset" | "legend" => "div",
                    _ => "span",
                };
                let block = matches!(
                    output,
                    "div"
                        | "p"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "pre"
                        | "li"
                        | "tr"
                        | "blockquote"
                );
                if block {
                    self.text.push('\n');
                }
                self.html.push('<');
                self.html.push_str(output);
                if name == "a" {
                    if let Some(href) = attr(node, "href").and_then(|href| self.url(&href)) {
                        self.html.push_str(" href=\"");
                        self.html.push_str(&escape(href.as_str()));
                        self.html.push('"');
                    }
                }
                if matches!(name, "code" | "pre") {
                    if let Some(language) = attr(node, "class").and_then(|classes| {
                        classes
                            .split_whitespace()
                            .find(|class| {
                                class.starts_with("language-") || class.starts_with("lang-")
                            })
                            .map(str::to_owned)
                    }) {
                        self.html.push_str(" class=\"");
                        self.html.push_str(&escape(&language));
                        self.html.push('"');
                    }
                }
                for key in ["colspan", "rowspan"] {
                    if let Some(value) = attr(node, key).filter(|value| {
                        value.len() <= 3 && value.chars().all(|c| c.is_ascii_digit())
                    }) {
                        self.html.push_str(&format!(" {key}=\"{value}\""));
                    }
                }
                self.html.push('>');
                for child in node.children.borrow().iter() {
                    self.node(child, depth + 1);
                }
                if !matches!(output, "br" | "hr") {
                    self.html.push_str("</");
                    self.html.push_str(output);
                    self.html.push('>');
                }
                if block || output == "br" {
                    self.text.push('\n');
                }
            }
            NodeData::Document => {
                for child in node.children.borrow().iter() {
                    self.node(child, depth + 1);
                }
            }
            _ => {}
        }
    }
    fn url(&self, raw: &str) -> Option<url::Url> {
        let raw = clean_location(raw);
        let url = resolve_url(self.source, self.documents, &raw).ok()?;
        match url.scheme() {
            "http" | "https"
                if url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none() =>
            {
                Some(url)
            }
            "file" => {
                local_file(&url, self.documents).ok()?;
                Some(url)
            }
            _ => None,
        }
    }
    fn image(&mut self, node: &Handle) {
        let alt = attr(node, "alt").unwrap_or_default();
        if let Some((url, path)) = attr(node, "src")
            .and_then(|raw| self.url(&raw))
            .filter(|url| url.scheme() == "file")
            .and_then(|url| {
                let path = local_file(&url, self.documents).ok()?;
                let meta = path.metadata().ok()?;
                (meta.len() <= 8 * 1024 * 1024).then_some((url, path))
            })
        {
            self.html.push_str(&format!(
                "<img src=\"{}\" alt=\"{}\">",
                escape(url.as_str()),
                escape(&alt)
            ));
            self.images.insert(url.into(), path);
        } else if !alt.is_empty() {
            self.html.push_str(&format!("<p>{}</p>", escape(&alt)));
        }
        if !alt.is_empty() {
            self.text.push_str(&alt);
            self.text.push('\n');
        }
    }
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn resolve_url(base: &url::Url, documents: &Path, raw: &str) -> Result<url::Url> {
    if raw.starts_with("//") {
        return Ok(url::Url::parse(&format!("https:{raw}"))?);
    }
    if raw.starts_with('/') {
        let base = url::Url::from_directory_path(documents)
            .map_err(|_| anyhow::anyhow!("文档目录无效"))?;
        return Ok(base.join(raw.trim_start_matches('/'))?);
    }
    Ok(base.join(raw)?)
}

// Check the lexical URL before touching the filesystem, then canonicalize to
// guard against symlinks. In particular, never probe a file:// network share.
fn local_file(url: &url::Url, documents: &Path) -> Result<PathBuf> {
    ensure!(
        url.scheme() == "file" && url.host_str().is_none(),
        "链接超出了当前文档集"
    );
    let base =
        url::Url::from_directory_path(documents).map_err(|_| anyhow::anyhow!("文档目录无效"))?;
    let candidate = if cfg!(windows) {
        url.path().to_lowercase()
    } else {
        url.path().to_owned()
    };
    let prefix = if cfg!(windows) {
        base.path().to_lowercase()
    } else {
        base.path().to_owned()
    };
    let encoded = candidate.to_ascii_lowercase();
    ensure!(
        candidate.starts_with(&prefix) && !encoded.contains("%2f") && !encoded.contains("%5c"),
        "链接超出了当前文档集"
    );
    let path = url
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("文档链接无效"))?
        .canonicalize()
        .context("链接指向的本地文档不存在")?;
    ensure!(
        path.starts_with(documents) && path.is_file(),
        "链接超出了当前文档集"
    );
    Ok(path)
}
