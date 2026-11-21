use super::*;
use rusqlite::{Connection, params};
use std::time::{Duration, Instant};

fn wait(manager: &Manager) {
    let until = Instant::now() + Duration::from_secs(8);
    while manager.0.busy.load(Ordering::Acquire) || manager.state().progress.is_some() {
        assert!(Instant::now() < until, "docset worker timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        manager.state().error.is_none(),
        "{:?}",
        manager.state().error
    );
}
fn fixture(home: &Path, key: &str, legacy: bool) -> Installed {
    let entry = Available::from_key(key.into()).unwrap();
    let directory = format!("{}-{}", entry.hash(), nonce());
    let relative = PathBuf::from("payload/Fixture.docset");
    let bundle = home.join("docsets/sets").join(&directory).join(&relative);
    let resources = bundle.join("Contents/Resources");
    let documents = resources.join("Documents");
    fs::create_dir_all(&documents).unwrap();
    fs::write(
        documents.join("index.html"),
        "<!doctype html><h1>Offline fixture</h1>",
    )
    .unwrap();
    fs::write(
        documents.join("类型 文档.html"),
        "<!doctype html><h1 id='new'>中文文档</h1>",
    )
    .unwrap();
    let mut values = plist::Dictionary::new();
    values.insert("CFBundleName".into(), plist::Value::String(key.into()));
    values.insert(
        "dashIndexFilePath".into(),
        plist::Value::String("index.html".into()),
    );
    plist::Value::Dictionary(values)
        .to_file_xml(bundle.join("Contents/Info.plist"))
        .unwrap();
    let db = Connection::open(resources.join("docSet.dsidx")).unwrap();
    if legacy {
        db.execute_batch("CREATE TABLE ztoken(ztokenname TEXT,zmetainformation INTEGER,ztokentype INTEGER);CREATE TABLE ztokenmetainformation(z_pk INTEGER,zfile INTEGER,zanchor TEXT);CREATE TABLE zfilepath(z_pk INTEGER,zpath TEXT);CREATE TABLE ztokentype(z_pk INTEGER,ztypename TEXT);INSERT INTO ztoken VALUES('GET',1,1);INSERT INTO ztokenmetainformation VALUES(1,1,'//apple_ref/c/func/GET');INSERT INTO zfilepath VALUES(1,'index.html');INSERT INTO ztokentype VALUES(1,'Method');").unwrap();
    } else {
        db.execute_batch(
            "CREATE TABLE searchIndex(id INTEGER PRIMARY KEY,name TEXT,type TEXT,path TEXT)",
        )
        .unwrap();
        for (name, path) in [
            ("Vec::new", "类型%20文档.html#new"),
            ("Vec::push", "index.html#push"),
            ("100%_literal", "index.html#literal"),
        ] {
            db.execute(
                "INSERT INTO searchIndex(name,type,path) VALUES(?1,'Function',?2)",
                params![name, path],
            )
            .unwrap();
        }
    }
    drop(db);
    let mut docset = download::inspect_bundle(&bundle, &entry, "test-1", 300).unwrap();
    docset.directory = directory;
    docset.bundle = relative;
    docset
}
fn manager(home: &Path, documents: Vec<Installed>) -> Manager {
    write_json(&home.join("docsets/installed.json"), &documents).unwrap();
    let manager = Manager::new(home);
    wait(&manager);
    manager
}

#[test]
fn search_supports_both_dash_indexes_scoping_literals_and_unicode_anchors() {
    let home = tempfile::tempdir().unwrap();
    let rust = fixture(home.path(), "Rust", false);
    let http = fixture(home.path(), "HTTP", true);
    let manager = manager(home.path(), vec![rust, http]);
    let report = manager.search("rust: Vec", 20).unwrap();
    assert_eq!(report.hits.len(), 2);
    assert!(report.warning.is_none(), "{:?}", report.warning);
    let hit = report
        .hits
        .iter()
        .find(|hit| hit.title == "Vec::new")
        .unwrap();
    let url = manager.resolve(hit).unwrap();
    assert_eq!(url.fragment(), Some("new"));
    assert!(url.to_file_path().unwrap().is_file());
    assert_eq!(manager.search("rust　Vec", 20).unwrap().hits.len(), 2);
    assert_eq!(
        manager.search("http GET", 20).unwrap().hits[0].kind,
        "Method"
    );
    assert_eq!(
        manager.search("%_", 20).unwrap().hits[0].title,
        "100%_literal"
    );
    let mut escaped = hit.clone();
    escaped.path = "%2e%2e/Info.plist".into();
    assert!(manager.resolve(&escaped).is_err());
    assert!(manager.search("", 20).unwrap().hits.len() == 2);
}

#[test]
fn enable_remove_and_install_metadata_survive_reopening() {
    let home = tempfile::tempdir().unwrap();
    let rust = fixture(home.path(), "Rust", false);
    let path = rust.bundle_path(&home.path().join("docsets")).unwrap();
    let manager = manager(home.path(), vec![rust]);
    manager.enable("Rust", false).unwrap();
    wait(&manager);
    assert!(manager.search("Vec", 20).unwrap().hits.is_empty());
    assert_eq!(manager.search("rust: Vec", 20).unwrap().hits.len(), 2);
    let reopened = Manager::new(home.path());
    wait(&reopened);
    assert!(!reopened.state().installed[0].enabled);
    reopened.remove("Rust").unwrap();
    wait(&reopened);
    assert!(!path.exists());
    assert!(reopened.state().installed.is_empty());
    assert!(remove_owned(&home.path().join("docsets"), "../outside").is_err());
}

#[test]
fn offline_reader_serves_html_and_rejects_paths_outside_docsets() {
    let home = tempfile::tempdir().unwrap();
    let document = fixture(home.path(), "Rust", false);
    let manager = manager(home.path(), vec![document]);
    let hit = manager.search("rust Vec::new", 10).unwrap().hits.remove(0);
    let url = manager.page_url(&hit).unwrap();
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert_eq!(url.fragment(), Some("new"));
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .timeout(Duration::from_secs(3))
        .build();
    let response = agent.get(url.as_str()).call().unwrap();
    assert!(
        response
            .header("content-type")
            .unwrap()
            .starts_with("text/html")
    );
    assert!(response.into_string().unwrap().contains("中文文档"));
    let address = url.socket_addrs(|| None).unwrap()[0];
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let token = url.path_segments().unwrap().next().unwrap();
    write!(stream,"GET /{token}/%2e%2e/%2e%2e/settings.json HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 404"));
}

#[test]
fn textview_reader_locates_rust_and_sphinx_functions_and_retains_code() {
    let home = tempfile::tempdir().unwrap();
    let document = fixture(home.path(), "Rust", false);
    let documents = document
        .bundle_path(&home.path().join("docsets"))
        .unwrap()
        .join("Contents/Resources/Documents");
    let manager = manager(home.path(), vec![document]);
    let hit = manager.search("rust Vec::new", 10).unwrap().hits.remove(0);
    fs::write(documents.join("类型 文档.html"),r##"<html><head><style>body{color:red}</style><script>do_not_render()</script></head><body><nav>Navigation chrome</nav><main><h1>Vec</h1><details><summary><section id="new"><h4><code>pub fn new() -&gt; Vec</code></h4></section></summary><div class="docblock"><p>Creates an empty vector.</p><pre><code class="language-rust">let mut v = Vec::new();
  v.push(1);</code></pre><a href="#push">push</a><table><tr><th>Value</th></tr><tr><td>one</td></tr></table></div></details><details><summary><section id="push"><h4>push</h4></section></summary><p>Unrelated method body</p></details></main></body></html>"##).unwrap();
    let page = manager.read_page(&hit, false).unwrap();
    assert!(page.scoped);
    assert!(!page.full_page);
    assert!(page.text.contains("Creates an empty vector"));
    assert!(page.html.contains("  v.push(1);"));
    assert!(page.html.contains("language-rust"));
    assert!(page.html.contains("<table>"));
    assert!(!page.html.contains("do_not_render"));
    assert!(!page.text.contains("Navigation chrome"));
    assert!(!page.text.contains("Unrelated method body"));
    let full = manager.read_page(&hit, true).unwrap();
    assert!(full.full_page);
    assert!(!full.scoped);
    assert!(full.text.contains("Unrelated method body"));
    fs::write(documents.join("类型 文档.html"),"<main><dl><dt id='new'>new()</dt><dd><p>Definition body</p></dd><dt id='other'>other()</dt><dd>Other definition</dd></dl></main>").unwrap();
    let page = manager.read_page(&hit, false).unwrap();
    assert!(page.text.contains("Definition body"));
    assert!(!page.text.contains("Other definition"));
}

#[test]
fn textview_reader_links_stay_local_and_images_are_confined_to_the_docset() {
    let home = tempfile::tempdir().unwrap();
    let document = fixture(home.path(), "Rust", false);
    let documents = document
        .bundle_path(&home.path().join("docsets"))
        .unwrap()
        .join("Contents/Resources/Documents");
    let manager = manager(home.path(), vec![document]);
    let hit = manager.search("rust Vec::new", 10).unwrap().hits.remove(0);
    fs::write(documents.join("diagram.png"), b"fixture image").unwrap();
    fs::write(documents.join("类型 文档.html"),"<main><section id='new'><h2>new</h2><p>Read <a href='/index.html#push'>push</a></p><img src='diagram.png' alt='Local diagram'><img src='https://example.com/remote.png' alt='Remote diagram'><a href='javascript:run()'>unsafe</a></section></main>").unwrap();
    let page = manager.read_page(&hit, false).unwrap();
    assert_eq!(page.images.len(), 1);
    assert!(page.html.contains("file:///"));
    assert!(!page.html.contains("remote.png"));
    assert!(!page.html.contains("javascript:"));
    let LinkTarget::Local(next) = manager.follow_link(&hit, "/index.html#push").unwrap() else {
        panic!("local link must stay in the reader");
    };
    assert_eq!(next.path, "index.html#push");
    assert!(
        manager
            .read_page(&next, false)
            .unwrap()
            .text
            .contains("Offline fixture")
    );
    assert!(manager.follow_link(&hit, "../../Info.plist").is_err());
    assert!(manager.follow_link(&hit, "javascript:run()").is_err());
    assert!(
        manager
            .follow_link(&hit, "file://unreachable.invalid/share/page.html")
            .is_err()
    );
    fs::write(
        documents.join("100%#literal.html"),
        "<h1 id='here'>Literal filename</h1>",
    )
    .unwrap();
    let LinkTarget::Local(literal) = manager
        .follow_link(&hit, "100%25%23literal.html?view=full#here")
        .unwrap()
    else {
        panic!("expected local link");
    };
    assert!(
        manager
            .read_page(&literal, false)
            .unwrap()
            .text
            .contains("Literal filename")
    );
    assert!(matches!(
        manager
            .follow_link(&hit, "https://example.com/guide")
            .unwrap(),
        LinkTarget::External(_)
    ));
}

#[test]
fn textview_reader_bounds_large_pages_and_reports_missing_anchors() {
    let home = tempfile::tempdir().unwrap();
    let document = fixture(home.path(), "Rust", false);
    let documents = document
        .bundle_path(&home.path().join("docsets"))
        .unwrap()
        .join("Contents/Resources/Documents");
    let manager = manager(home.path(), vec![document]);
    let hit = manager.search("rust Vec::new", 10).unwrap().hits.remove(0);
    fs::write(
        documents.join("类型 文档.html"),
        format!("<main><p>{}</p></main>", "字".repeat(110_000)),
    )
    .unwrap();
    let page = manager.read_page(&hit, false).unwrap();
    assert!(page.notice.as_ref().unwrap().contains("未找到"));
    assert!(page.notice.as_ref().unwrap().contains("截取"));
    assert!(page.text.chars().count() < 100_000);
    fs::write(
        documents.join("类型 文档.html"),
        "<html><body><script>client_render()</script><div id='root'></div></body></html>",
    )
    .unwrap();
    assert!(manager.read_page(&hit, false).is_err());
}

#[test]
#[ignore = "set MOIN_DOCSET_SMOKE_ARCHIVE to a previously downloaded official HTTP docset archive"]
fn official_cached_docset_html_is_readable_in_textview_projection() {
    let archive = PathBuf::from(
        std::env::var_os("MOIN_DOCSET_SMOKE_ARCHIVE").expect("set MOIN_DOCSET_SMOKE_ARCHIVE"),
    );
    let home = tempfile::tempdir().unwrap();
    let entry = Available::from_key("HTTP".into()).unwrap();
    let directory = format!("{}-{}", entry.hash(), nonce());
    let base = home.path().join("docsets/sets").join(&directory);
    fs::create_dir_all(&base).unwrap();
    let (bundle, size) = download::unpack(&archive, &base, &AtomicBool::new(false)).unwrap();
    let mut document = download::inspect_bundle(&bundle, &entry, "fixture", size).unwrap();
    document.directory = directory;
    document.bundle = bundle.strip_prefix(&base).unwrap().to_owned();
    let manager = manager(home.path(), vec![document]);
    let hit = manager.search("http: GET", 20).unwrap().hits.remove(0);
    let page = manager.read_page(&hit, false).unwrap();
    assert!(!page.text.trim().is_empty());
    assert!(!page.html.contains("<script"));
    eprintln!(
        "Official HTTP {}: {} readable characters, function scope {}",
        hit.title,
        page.text.chars().count(),
        page.scoped
    );
}

#[test]
#[ignore = "downloads a public Kapeli docset into a temporary directory; opt-in network test"]
fn official_download_install_search_and_local_reader() {
    let home = tempfile::tempdir().unwrap();
    let manager = Manager::new(home.path());
    wait(&manager);
    manager.install("HTTP").unwrap();
    let until = Instant::now() + Duration::from_secs(900);
    let mut last_report = Instant::now();
    while manager.0.busy.load(Ordering::Acquire) || manager.state().progress.is_some() {
        if last_report.elapsed() > Duration::from_secs(5) {
            if let Some(progress) = manager.state().progress {
                eprintln!("{}: {} bytes", progress.stage, progress.received);
            }
            last_report = Instant::now();
        }
        assert!(
            Instant::now() < until,
            "official install timed out: {:?}",
            manager.state().progress
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let state = manager.state();
    assert!(state.error.is_none(), "{:?}", state.error);
    assert_eq!(state.installed.len(), 1);
    assert!(state.installed[0].symbols > 0);
    let report = manager.search("http: GET", 30).unwrap();
    assert!(!report.hits.is_empty(), "{report:?}");
    let url = manager.page_url(&report.hits[0]).unwrap();
    let response = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .timeout(Duration::from_secs(5))
        .build()
        .get(url.as_str())
        .call()
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response
            .header("content-type")
            .unwrap()
            .starts_with("text/html")
    );
    eprintln!(
        "Kapeli HTTP {}: {} indexed entries; installed, searched and served offline",
        state.installed[0].version, state.installed[0].symbols
    );
    manager.remove("HTTP").unwrap();
    wait(&manager);
    assert!(manager.state().installed.is_empty());
}
