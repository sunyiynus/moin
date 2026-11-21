//! Subscription storage and bounded access to the local Mihomo controller.
use crate::config::MihomoConfig;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serde_yaml::Value as Yaml;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_PROFILE: u64 = 8 * 1024 * 1024;
const MAX_JSON: u64 = 16 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SubscriptionUsage {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    pub expire: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProfileSource {
    Remote { url: String },
    Local { path: PathBuf },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub source: ProfileSource,
    pub updated: u64,
    pub proxies: usize,
    pub groups: usize,
    #[serde(default)]
    pub usage: Option<SubscriptionUsage>,
}
impl Profile {
    pub fn remote(&self) -> bool {
        matches!(self.source, ProfileSource::Remote { .. })
    }
    pub fn origin(&self) -> String {
        match &self.source {
            ProfileSource::Remote { url } => url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "远程订阅".into()),
            ProfileSource::Local { path } => path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "本地配置".into()),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Catalog {
    pub selected: Option<String>,
    pub profiles: Vec<Profile>,
}

fn directory(home: &Path) -> PathBuf {
    home.join("mihomo").join("profiles")
}
fn profile_path(home: &Path, id: &str, suffix: &str) -> Result<PathBuf> {
    ensure!(
        !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "配置标识无效"
    );
    Ok(directory(home).join(format!("{id}.{suffix}.yaml")))
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path).context("无法读取配置文件")?;
    let mut bytes = vec![];
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "配置文件过大（上限 8 MiB）");
    Ok(bytes)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("保存路径无效")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".moin-{}-{}.part",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.context("无法保存 Mihomo 配置")
}

fn parse_profile(bytes: &[u8]) -> Result<(Yaml, usize, usize)> {
    ensure!(
        bytes.len() as u64 <= MAX_PROFILE,
        "配置文件过大（上限 8 MiB）"
    );
    let value: Yaml = serde_yaml::from_slice(bytes)
        .context("不是有效的 Mihomo YAML 配置，请使用 Clash / Mihomo 订阅链接")?;
    let map = value
        .as_mapping()
        .context("配置需要是 YAML 对象，不能是节点链接、Base64 或 HTML")?;
    ensure!(
        [
            "proxies",
            "proxy-providers",
            "proxy-groups",
            "rules",
            "mixed-port",
            "port",
            "dns",
            "listeners"
        ]
        .iter()
        .any(|k| map.contains_key(Yaml::String((*k).into()))),
        "未找到 Mihomo 配置内容，请确认订阅格式为 Clash / Mihomo"
    );
    let proxies = value
        .get("proxies")
        .and_then(Yaml::as_sequence)
        .map_or(0, Vec::len);
    let groups = value
        .get("proxy-groups")
        .and_then(Yaml::as_sequence)
        .map_or(0, Vec::len);
    Ok((value, proxies, groups))
}

fn validate_local_controller(value: &Yaml, config: &MihomoConfig) -> Result<()> {
    let target = url::Url::parse(&config.controller)?;
    let bind = value
        .get("external-controller")
        .and_then(Yaml::as_str)
        .context("本地配置缺少 external-controller，请将其与内核设置中的控制器地址保持一致")?;
    let bind = url::Url::parse(&format!("http://{bind}"))
        .context("本地配置的 external-controller 无效")?;
    let compatible_host = bind.host() == target.host()
        || bind.host_str() == Some("localhost")
        || match (bind.host(), target.host()) {
            (Some(url::Host::Ipv4(address)), Some(url::Host::Ipv4(_))) => address.is_unspecified(),
            (Some(url::Host::Ipv6(address)), Some(url::Host::Ipv6(_))) => address.is_unspecified(),
            _ => false,
        };
    ensure!(
        compatible_host && bind.port() == target.port(),
        "本地配置的 external-controller 与内核设置不一致，请先调整后再加载"
    );
    ensure!(
        value
            .get("secret")
            .and_then(Yaml::as_str)
            .unwrap_or_default()
            == config.secret,
        "本地配置的 Secret 与内核设置不一致，请先调整后再加载"
    );
    Ok(())
}

impl Catalog {
    pub fn load(home: &Path) -> Result<Self> {
        let path = directory(home).join("profiles.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let catalog: Self =
            serde_json::from_slice(&read_bounded(&path, MAX_PROFILE)?).context("订阅列表损坏")?;
        for p in &catalog.profiles {
            let _ = profile_path(home, &p.id, "source")?;
        }
        Ok(catalog)
    }
    pub fn save(&self, home: &Path) -> Result<()> {
        atomic_write(
            &directory(home).join("profiles.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
    pub fn import_remote(&mut self, home: &Path, name: &str, address: &str) -> Result<String> {
        let url = subscription_url(address)?;
        let (bytes, usage) = download(url.as_str())?;
        self.add(
            home,
            name,
            ProfileSource::Remote { url: url.into() },
            bytes,
            usage,
        )
    }
    pub fn import_local(&mut self, home: &Path, name: &str, path: &Path) -> Result<String> {
        ensure!(
            path.is_absolute() && path.is_file(),
            "请选择有效的本地配置文件"
        );
        self.add(
            home,
            name,
            ProfileSource::Local {
                path: path.to_owned(),
            },
            read_bounded(path, MAX_PROFILE)?,
            None,
        )
    }
    fn add(
        &mut self,
        home: &Path,
        name: &str,
        source: ProfileSource,
        bytes: Vec<u8>,
        usage: Option<SubscriptionUsage>,
    ) -> Result<String> {
        let (_, proxies, groups) = parse_profile(&bytes)?;
        ensure!(self.profiles.len() < 200, "配置数量已达到 200 个");
        let id = format!(
            "{:x}{:x}{:x}",
            now(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mut name = name.trim().chars().take(80).collect::<String>();
        if name.is_empty() {
            name = match &source {
                ProfileSource::Remote { .. } => format!("订阅 {}", self.profiles.len() + 1),
                ProfileSource::Local { path } => path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            };
        }
        let path = profile_path(home, &id, "source")?;
        atomic_write(&path, &bytes)?;
        self.profiles.push(Profile {
            id: id.clone(),
            name,
            source,
            updated: now(),
            proxies,
            groups,
            usage,
        });
        if let Err(error) = self.save(home) {
            self.profiles.pop();
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(id)
    }
    pub fn update(&mut self, home: &Path, id: &str) -> Result<()> {
        let index = self
            .profiles
            .iter()
            .position(|p| p.id == id)
            .context("配置不存在")?;
        let (bytes, usage) = match &self.profiles[index].source {
            ProfileSource::Remote { url } => download(subscription_url(url)?.as_str())?,
            ProfileSource::Local { path } => (read_bounded(path, MAX_PROFILE)?, None),
        };
        let (_, proxies, groups) = parse_profile(&bytes)?;
        let path = profile_path(home, id, "source")?;
        let old_bytes = read_bounded(&path, MAX_PROFILE)?;
        let previous = self.profiles[index].clone();
        atomic_write(&path, &bytes)?;
        self.profiles[index].proxies = proxies;
        self.profiles[index].groups = groups;
        self.profiles[index].usage = usage;
        self.profiles[index].updated = now();
        if let Err(error) = self.save(home) {
            self.profiles[index] = previous;
            atomic_write(&path, &old_bytes)?;
            return Err(error);
        }
        Ok(())
    }
    pub fn remove(&mut self, home: &Path, id: &str) -> Result<()> {
        ensure!(
            self.selected.as_deref() != Some(id),
            "请先切换到其他配置，再删除当前配置"
        );
        let index = self
            .profiles
            .iter()
            .position(|p| p.id == id)
            .context("配置不存在")?;
        let profile = self.profiles.remove(index);
        if let Err(error) = self.save(home) {
            self.profiles.insert(index, profile);
            return Err(error);
        }
        for suffix in ["source", "runtime"] {
            let _ = fs::remove_file(profile_path(home, id, suffix)?);
        }
        if let Ok(entries) = fs::read_dir(directory(home)) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&format!("{id}.runtime-")) && name.ends_with(".yaml") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }
    /// Build a managed runtime file; the original subscription is never edited.
    pub fn prepare(&self, home: &Path, id: &str, config: &MihomoConfig) -> Result<PathBuf> {
        config.validate_controller()?;
        let p = self
            .profiles
            .iter()
            .find(|p| p.id == id)
            .context("配置不存在")?;
        // Local profiles keep their original directory for provider and geodata paths.
        if let ProfileSource::Local { path } = &p.source {
            let (value, _, _) = parse_profile(&read_bounded(path, MAX_PROFILE)?)?;
            validate_local_controller(&value, config)?;
            return Ok(path.clone());
        }
        let (mut value, _, _) = parse_profile(&read_bounded(
            &profile_path(home, id, "source")?,
            MAX_PROFILE,
        )?)?;
        let url = url::Url::parse(&config.controller)?;
        let host = url.host_str().context("控制器地址无效")?;
        let host = host.trim_matches(['[', ']']);
        let endpoint = if host.contains(':') {
            format!("[{host}]:{}", url.port().context("控制器端口缺失")?)
        } else {
            format!("{host}:{}", url.port().context("控制器端口缺失")?)
        };
        let map = value.as_mapping_mut().unwrap();
        map.insert(
            Yaml::String("external-controller".into()),
            Yaml::String(endpoint),
        );
        map.insert(
            Yaml::String("secret".into()),
            Yaml::String(config.secret.clone()),
        );
        // Remote subscriptions cannot replace the local management transport.
        for key in [
            "external-controller-tls",
            "external-controller-unix",
            "external-controller-pipe",
        ] {
            map.remove(Yaml::String(key.into()));
        }
        // New versions are immutable: a failed reload must not overwrite the
        // configuration used by the running core or by the next application start.
        let suffix = format!(
            "runtime-{:x}-{:x}",
            now(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = profile_path(home, id, &suffix)?;
        atomic_write(&path, serde_yaml::to_string(&value)?.as_bytes())?;
        Ok(path)
    }
    pub fn select(&mut self, home: &Path, id: &str) -> Result<()> {
        ensure!(self.profiles.iter().any(|p| p.id == id), "配置不存在");
        let old = self.selected.replace(id.to_owned());
        if let Err(error) = self.save(home) {
            self.selected = old;
            return Err(error);
        }
        Ok(())
    }
}

pub fn discard_runtime(home: &Path, id: &str, path: &Path) {
    if profile_path(home, id, "source").is_err() {
        return;
    }
    let own = path.parent() == Some(directory(home).as_path())
        && path.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with(&format!("{id}.runtime-")) && n.ends_with(".yaml")
        });
    if own {
        let _ = fs::remove_file(path);
    }
}

fn subscription_url(address: &str) -> Result<url::Url> {
    let url = url::Url::parse(address.trim()).context("请输入完整的 HTTP / HTTPS 订阅链接")?;
    ensure!(
        matches!(url.scheme(), "https" | "http")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "订阅链接只支持 HTTP / HTTPS，不能包含账号密码"
    );
    Ok(url)
}
fn download(address: &str) -> Result<(Vec<u8>, Option<SubscriptionUsage>)> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .redirects(5)
        .build()
        .get(address)
        .set("User-Agent", "clash.meta/moin")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(status, _) => anyhow::anyhow!("订阅下载失败：HTTP {status}"),
            _ => anyhow::anyhow!("订阅下载失败，请检查网络和订阅链接"),
        })?;
    let usage = response.header("subscription-userinfo").map(|s| {
        let mut usage = SubscriptionUsage::default();
        for field in s.split(';') {
            if let Some((k, v)) = field.trim().split_once('=') {
                if let Ok(v) = v.trim().parse() {
                    match k.trim() {
                        "upload" => usage.upload = v,
                        "download" => usage.download = v,
                        "total" => usage.total = v,
                        "expire" => usage.expire = v,
                        _ => {}
                    }
                }
            }
        }
        usage
    });
    let mut bytes = vec![];
    response
        .into_reader()
        .take(MAX_PROFILE + 1)
        .read_to_end(&mut bytes)
        .context("无法读取订阅内容")?;
    parse_profile(&bytes)?;
    Ok((bytes, usage))
}

#[derive(Clone)]
pub struct Controller {
    agent: ureq::Agent,
    config: MihomoConfig,
}
impl Controller {
    pub fn new(config: &MihomoConfig) -> Result<Self> {
        config.validate_controller()?;
        Ok(Self {
            config: config.clone(),
            agent: ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .redirects(0)
                .timeout(Duration::from_secs(8))
                .build(),
        })
    }
    pub fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        ensure!(
            path.starts_with('/') && !path.starts_with("//"),
            "控制器路径无效"
        );
        let endpoint = format!("{}{path}", self.config.controller.trim_end_matches('/'));
        let mut request = self.agent.request(method, &endpoint);
        if !self.config.secret.is_empty() {
            request = request.set("Authorization", &format!("Bearer {}", self.config.secret));
        }
        let response = match body {
            Some(body) => request.send_json(body),
            None => request.call(),
        }
        .map_err(|e| match e {
            ureq::Error::Status(401 | 403, _) => {
                anyhow::anyhow!("控制器鉴权失败，请检查内核设置中的 Secret")
            }
            ureq::Error::Status(400 | 422, _) => {
                anyhow::anyhow!("核心拒绝了配置或操作，请检查配置内容和核心日志")
            }
            ureq::Error::Status(status, _) => {
                anyhow::anyhow!("控制器返回 HTTP {status}，请检查核心状态与日志")
            }
            _ => anyhow::anyhow!("无法连接 Mihomo 控制器，请确认核心已启动"),
        })?;
        ensure!(
            (200..300).contains(&response.status()),
            "本地控制器返回了重定向，操作未执行"
        );
        if response.status() == 204 {
            return Ok(Value::Null);
        }
        let mut bytes = vec![];
        response
            .into_reader()
            .take(MAX_JSON + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_JSON, "控制器响应过大");
        if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_slice(&bytes).context("控制器返回了无效 JSON")
        }
    }
    pub fn reload(&self, path: &Path) -> Result<()> {
        let bytes = read_bounded(path, MAX_PROFILE)?;
        parse_profile(&bytes)?;
        let payload = String::from_utf8(bytes).context("配置需要使用 UTF-8 编码")?;
        self.request(
            "PUT",
            "/configs?force=true",
            Some(json!({"payload":payload})),
        )?;
        Ok(())
    }
    pub fn config(&self) -> Result<Value> {
        self.request("GET", "/configs", None)
    }
    pub fn patch(&self, updates: Value) -> Result<()> {
        self.request("PATCH", "/configs", Some(updates))?;
        Ok(())
    }
    pub fn proxies(&self) -> Result<Value> {
        self.request("GET", "/proxies", None)
    }
    pub fn rules(&self) -> Result<Value> {
        self.request("GET", "/rules", None)
    }
    pub fn connections(&self) -> Result<Value> {
        self.request("GET", "/connections", None)
    }
    pub fn select_proxy(&self, group: &str, name: &str) -> Result<()> {
        self.request(
            "PUT",
            &format!("/proxies/{}", segment(group)),
            Some(json!({"name":name})),
        )?;
        Ok(())
    }
    pub fn delay(&self, name: &str) -> Result<u64> {
        Ok(self.request("GET",&format!("/proxies/{}/delay?timeout=5000&url=https%3A%2F%2Fwww.gstatic.com%2Fgenerate_204",segment(name)),None)?.get("delay").and_then(Value::as_u64).context("节点测速失败")?)
    }
    pub fn close(&self, id: Option<&str>) -> Result<()> {
        self.request(
            "DELETE",
            &id.map_or_else(
                || "/connections".into(),
                |id| format!("/connections/{}", segment(id)),
            ),
            None,
        )?;
        Ok(())
    }
}
fn segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

pub fn log_tail(home: &Path) -> Result<String> {
    use std::io::{Seek, SeekFrom};
    let path = home.join("mihomo/logs/mihomo.log");
    if !path.exists() {
        return Ok("核心尚未生成日志。".into());
    }
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    file.seek(SeekFrom::Start(size.saturating_sub(64 * 1024)))?;
    let mut bytes = vec![];
    file.take(64 * 1024).read_to_end(&mut bytes)?;
    let bytes = if size > 64 * 1024 {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(&[][..], |index| &bytes[index + 1..])
    } else {
        &bytes[..]
    };
    Ok(String::from_utf8_lossy(bytes)
        .lines()
        .rev()
        .take(200)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in smoke test against an installed core. Uses a disposable home,
    /// no inbound proxy ports, no DNS/TUN and no external test traffic.
    #[test]
    #[ignore = "set MOIN_MIHOMO_TEST_BINARY to an installed Mihomo executable"]
    fn installed_core_profile_and_controller_smoke_test() {
        use std::{
            net::TcpListener,
            process::{Child, Command, Stdio},
            time::Instant,
        };
        struct Core(Child);
        impl Drop for Core {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let binary =
            std::env::var_os("MOIN_MIHOMO_TEST_BINARY").expect("set MOIN_MIHOMO_TEST_BINARY");
        let port = {
            let socket = TcpListener::bind("127.0.0.1:0").unwrap();
            socket.local_addr().unwrap().port()
        };
        let home = tempfile::tempdir().unwrap();
        let yaml = "port: 0\nsocks-port: 0\nmixed-port: 0\nallow-lan: false\nmode: rule\nlog-level: warning\nfind-process-mode: off\ntun: {enable: false}\ndns: {enable: false}\nproxies: []\nproxy-groups:\n  - name: '节点 选择/组'\n    type: select\n    proxies: [DIRECT, REJECT]\nrules: ['MATCH,DIRECT']\n";
        let (address, request) = server("200 OK", yaml, "");
        let mut catalog = Catalog::default();
        let id = catalog
            .import_remote(home.path(), "隔离测试", &address)
            .unwrap();
        request.join().unwrap();
        let config = MihomoConfig {
            controller: format!("http://127.0.0.1:{port}"),
            secret: "isolated-test-secret".into(),
            ..MihomoConfig::default()
        };
        let runtime = catalog.prepare(home.path(), &id, &config).unwrap();
        let log_path = home.path().join("core.log");
        let log = fs::File::create(&log_path).unwrap();
        let mut command = Command::new(binary);
        command
            .arg("-d")
            .arg(runtime.parent().unwrap())
            .arg("-f")
            .arg(&runtime)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut core = Core(command.spawn().unwrap());
        let client = Controller::new(&config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if client.config().is_ok() {
                break;
            }
            assert!(
                core.0.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "isolated core did not become ready: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            client
                .request("GET", "/version", None)
                .unwrap()
                .get("version")
                .is_some()
        );
        let proxies = client.proxies().unwrap();
        assert!(proxies["proxies"].get("节点 选择/组").is_some());
        client.select_proxy("节点 选择/组", "REJECT").unwrap();
        assert_eq!(
            client.proxies().unwrap()["proxies"]["节点 选择/组"]["now"],
            "REJECT"
        );
        client
            .patch(json!({"mode":"direct", "allow-lan":false}))
            .unwrap();
        assert_eq!(client.config().unwrap()["mode"], "direct");
        assert_eq!(
            client.rules().unwrap()["rules"].as_array().unwrap().len(),
            1
        );
        let connections = client.connections().unwrap();
        assert!(connections.get("uploadTotal").is_some());
        assert!(connections["connections"].is_null() || connections["connections"].is_array());
        client.close(None).unwrap();
        client.reload(&runtime).unwrap();
        assert_eq!(client.config().unwrap()["mode"], "rule");
        let invalid = home.path().join("invalid.yaml");
        fs::write(
            &invalid,
            "proxies: [{name: broken, type: nonexistent}]\nrules: ['MATCH,DIRECT']\n",
        )
        .unwrap();
        assert!(client.reload(&invalid).is_err());
        assert_eq!(client.config().unwrap()["mode"], "rule");
        assert!(
            client.proxies().unwrap()["proxies"]
                .get("节点 选择/组")
                .is_some()
        );
    }

    fn server(
        status: &str,
        body: &str,
        headers: &str,
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::Write;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        );
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = vec![];
            let mut buffer = [0; 2048];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
                assert!(request.len() < 1024 * 1024);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (address, thread)
    }
    #[test]
    fn remote_subscription_preserves_source_usage_and_immutable_runtime_versions() {
        let yaml = "mixed-port: 7890\nexternal-controller: 0.0.0.0:1234\nsecret: foreign\nproxies: []\nproxy-groups: []\nrules: ['MATCH,DIRECT']\n";
        let (address, request) = server(
            "200 OK",
            yaml,
            "subscription-userinfo: upload=12; download=34; total=100; expire=2000000000\r\n",
        );
        let home = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::default();
        let id = catalog
            .import_remote(
                home.path(),
                "中文订阅",
                &format!("{address}/subscription?token=private"),
            )
            .unwrap();
        assert!(
            request
                .join()
                .unwrap()
                .starts_with("GET /subscription?token=private HTTP/1.1")
        );
        assert_eq!(catalog.profiles[0].usage.as_ref().unwrap().download, 34);
        let mut config = MihomoConfig::default();
        config.secret = "local-secret".into();
        let first = catalog.prepare(home.path(), &id, &config).unwrap();
        let before = fs::read(&first).unwrap();
        let second = catalog.prepare(home.path(), &id, &config).unwrap();
        assert_ne!(first, second);
        let runtime: Yaml = serde_yaml::from_slice(&before).unwrap();
        assert_eq!(
            runtime["external-controller"].as_str(),
            Some("127.0.0.1:9090")
        );
        assert_eq!(runtime["secret"].as_str(), Some("local-secret"));
        assert_eq!(
            fs::read(profile_path(home.path(), &id, "source").unwrap()).unwrap(),
            yaml.as_bytes()
        );
        management_reject_test(&config, &second);
        discard_runtime(home.path(), &id, &second);
        assert!(!second.exists());
        assert_eq!(fs::read(&first).unwrap(), before);
        assert_eq!(Catalog::load(home.path()).unwrap(), catalog);
    }
    fn management_reject_test(config: &MihomoConfig, path: &Path) {
        let (address, request) = server("400 Bad Request", "{}", "");
        let mut config = config.clone();
        config.controller = address;
        assert!(Controller::new(&config).unwrap().reload(path).is_err());
        let request = request.join().unwrap();
        assert!(request.starts_with("PUT /configs?force=true HTTP/1.1"));
        assert!(request.contains("local-secret"));
    }
    #[test]
    fn controller_auth_unicode_selection_patch_and_redirect_rejection() {
        let (address, request) = server("204 No Content", "", "");
        let mut config = MihomoConfig::default();
        config.controller = address;
        config.secret = "test-secret".into();
        Controller::new(&config)
            .unwrap()
            .select_proxy("节点 选择/组", "日本节点")
            .unwrap();
        let request = request.join().unwrap();
        assert!(request.starts_with(
            "PUT /proxies/%E8%8A%82%E7%82%B9%20%E9%80%89%E6%8B%A9%2F%E7%BB%84 HTTP/1.1"
        ));
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer test-secret")
        );
        assert!(request.contains("日本节点"));
        let (address, request) = server("204 No Content", "", "");
        config.controller = address;
        Controller::new(&config)
            .unwrap()
            .patch(json!({"mode":"rule"}))
            .unwrap();
        assert!(
            request
                .join()
                .unwrap()
                .starts_with("PATCH /configs HTTP/1.1")
        );
        let (address, request) = server("302 Found", "", "Location: http://127.0.0.1:1/other\r\n");
        config.controller = address;
        assert!(Controller::new(&config).unwrap().config().is_err());
        let _ = request.join().unwrap();
    }
    #[test]
    fn local_catalog_roundtrips_and_remote_runtime_preserves_subscription() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("source.yaml");
        fs::write(
            &source,
            "external-controller: 127.0.0.1:9090\nmixed-port: 7890\nproxies: []\nrules: ['MATCH,DIRECT']\n",
        )
        .unwrap();
        let mut catalog = Catalog::default();
        let id = catalog.import_local(home.path(), "配置", &source).unwrap();
        assert_eq!(Catalog::load(home.path()).unwrap(), catalog);
        assert_eq!(
            catalog
                .prepare(home.path(), &id, &MihomoConfig::default())
                .unwrap(),
            source
        );
        catalog.select(home.path(), &id).unwrap();
        assert!(catalog.remove(home.path(), &id).is_err());
        catalog.selected = None;
        catalog.remove(home.path(), &id).unwrap();
        assert!(source.exists());
        assert!(profile_path(home.path(), "../outside", "source").is_err());
    }
    #[test]
    fn local_controller_mismatch_is_rejected_before_loading() {
        let config = MihomoConfig::default();
        let matching: Yaml =
            serde_yaml::from_str("external-controller: 0.0.0.0:9090\nsecret: ''").unwrap();
        assert!(validate_local_controller(&matching, &config).is_ok());
        let mismatched: Yaml =
            serde_yaml::from_str("external-controller: 127.0.0.1:9091\nsecret: ''").unwrap();
        assert!(validate_local_controller(&mismatched, &config).is_err());
        let secret: Yaml =
            serde_yaml::from_str("external-controller: 127.0.0.1:9090\nsecret: other").unwrap();
        assert!(validate_local_controller(&secret, &config).is_err());
    }
    #[test]
    fn rejects_non_config_and_preserves_old_data_on_invalid_update() {
        assert!(parse_profile(b"<html>login</html>").is_err());
        assert!(parse_profile(b"ss://node").is_err());
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("local.yaml");
        fs::write(&source, "proxies: []\nrules: []").unwrap();
        let mut catalog = Catalog::default();
        let id = catalog.import_local(home.path(), "测试", &source).unwrap();
        let original = fs::read(profile_path(home.path(), &id, "source").unwrap()).unwrap();
        fs::write(&source, "<html>error</html>").unwrap();
        assert!(catalog.update(home.path(), &id).is_err());
        assert_eq!(
            fs::read(profile_path(home.path(), &id, "source").unwrap()).unwrap(),
            original
        );
    }
}
