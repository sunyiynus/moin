//! Serve installed HTML and assets over loopback so JavaScript documentation
//! works offline without file:// restrictions. Only the managed docset tree is exposed.
use super::*;
use std::{
    io::{BufRead, BufReader},
    net::{TcpListener, TcpStream},
    sync::atomic::AtomicUsize,
    time::Duration,
};

pub(super) struct Server {
    base: url::Url,
    stop: Arc<AtomicBool>,
}
impl Server {
    pub(super) fn start(root: &Path) -> Result<Self> {
        let sets = root.canonicalize()?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        // One loopback origin per docset preserves absolute /assets/... links
        // and client-side routes used by modern documentation sites.
        let base = url::Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let clients = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, address)) if address.ip().is_loopback() => {
                        if clients.fetch_add(1, Ordering::AcqRel) >= 12 {
                            clients.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                        let (sets, clients) = (sets.clone(), clients.clone());
                        std::thread::spawn(move || {
                            struct Client(Arc<AtomicUsize>);
                            impl Drop for Client {
                                fn drop(&mut self) {
                                    self.0.fetch_sub(1, Ordering::AcqRel);
                                }
                            }
                            let _client = Client(clients);
                            let _ = serve(stream, &sets);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
        });
        Ok(Self { base, stop })
    }
    pub(super) fn page(&self, root: &Path, file: &url::Url) -> Result<url::Url> {
        let sets = root.canonicalize()?;
        let file_path = file
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("文档路径无效"))?
            .canonicalize()?;
        let relative = file_path.strip_prefix(sets)?;
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("本地文档服务地址无效"))?;
            segments.pop_if_empty();
            for part in relative.iter() {
                segments.push(&part.to_string_lossy());
            }
        }
        url.set_fragment(file.fragment());
        url.set_query(file.query());
        Ok(url)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn serve(mut stream: TcpStream, sets: &Path) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut input = BufReader::new(stream.try_clone()?.take(16 * 1024));
    let mut first = String::new();
    input.read_line(&mut first)?;
    let mut request = first.split_whitespace();
    let method = request.next().unwrap_or_default();
    let target = request.next().unwrap_or_default();
    if !matches!(method, "GET" | "HEAD") {
        return response(&mut stream, 405, "Method Not Allowed");
    }
    let mut host = String::new();
    let mut complete = false;
    let mut bytes = first.len();
    loop {
        let mut line = String::new();
        let n = input.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        bytes += n;
        if bytes >= 16 * 1024 {
            break;
        }
        if line == "\r\n" {
            complete = true;
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("host") {
                host = value.trim().to_owned();
            }
        }
    }
    if !complete || host != stream.local_addr()?.to_string() {
        return response(&mut stream, 400, "Bad Request");
    }
    let Some(raw) = target
        .split('?')
        .next()
        .unwrap_or_default()
        .strip_prefix('/')
    else {
        return response(&mut stream, 404, "Not Found");
    };
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8()?;
    let relative = match download::safe_relative(if decoded.is_empty() {
        "index.html"
    } else {
        &decoded
    }) {
        Ok(path) => path,
        Err(_) => return response(&mut stream, 404, "Not Found"),
    };
    let candidate = sets.join(relative);
    let candidate = if candidate.is_dir() {
        candidate.join("index.html")
    } else {
        candidate
    };
    let path = match candidate.canonicalize() {
        Ok(path) if path.starts_with(sets) && path.is_file() => path,
        _ => return response(&mut stream, 404, "Not Found"),
    };
    let file = fs::File::open(&path)?;
    let size = file.metadata()?.len();
    let mime = mime(&path);
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {size}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-cache\r\nContent-Security-Policy: default-src 'self' data: blob: 'unsafe-inline' 'unsafe-eval'; connect-src 'self'; frame-ancestors 'self'\r\n\r\n"
    )?;
    if method == "GET" {
        std::io::copy(&mut file.take(size), &mut stream)?;
    }
    Ok(())
}
fn response(stream: &mut TcpStream, status: u16, message: &str) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {message}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    Ok(())
}
fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
