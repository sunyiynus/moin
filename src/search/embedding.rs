use crate::config::EmbeddingConfig;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::Path, time::Duration};

const LOCAL_DIMENSIONS: usize = 384;
const MAX_DIMENSIONS: usize = 8192;
const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const BATCH_SIZE: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmbeddingProfile {
    LocalLexicalV1,
    OllamaV1 {
        model: String,
        endpoint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimensions: Option<usize>,
    },
}

impl EmbeddingProfile {
    pub fn label(&self) -> String {
        match self {
            Self::LocalLexicalV1 => "本地词法向量".into(),
            Self::OllamaV1 { model, .. } => format!("Ollama · {model}"),
        }
    }

    pub fn is_lexical(&self) -> bool {
        matches!(self, Self::LocalLexicalV1)
    }
}

#[derive(Deserialize)]
struct ConfigDocument {
    #[serde(default)]
    embedding: Option<EmbeddingConfig>,
}

pub struct Embeddings {
    profile: EmbeddingProfile,
    agent: Option<ureq::Agent>,
}

impl Embeddings {
    pub fn load(home: &Path) -> Result<Self> {
        let config_path = home.join("settings.json");
        if !config_path.is_file() {
            return Self::from_profile(EmbeddingProfile::LocalLexicalV1);
        }
        let text = fs::read_to_string(&config_path)
            .with_context(|| format!("无法读取 {}", config_path.display()))?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let document: ConfigDocument = serde_json::from_str(text)
            .with_context(|| format!("无法读取 {}", config_path.display()))?;
        match document.embedding {
            Some(config) => Self::from_profile(EmbeddingProfile::OllamaV1 {
                model: config.model,
                endpoint: config.endpoint,
                dimensions: config.dimensions,
            })
            .with_context(|| format!("{} 的 embedding 配置有误", config_path.display())),
            None => Self::from_profile(EmbeddingProfile::LocalLexicalV1),
        }
    }

    pub fn from_profile(mut profile: EmbeddingProfile) -> Result<Self> {
        let agent = match &mut profile {
            EmbeddingProfile::LocalLexicalV1 => None,
            EmbeddingProfile::OllamaV1 {
                model,
                endpoint,
                dimensions,
            } => {
                *model = model.trim().into();
                ensure!(
                    !model.is_empty()
                        && model.len() <= 255
                        && !model.chars().any(char::is_whitespace),
                    "Ollama 模型名称无效"
                );
                if let Some((_, tag)) = model
                    .rsplit('/')
                    .next()
                    .and_then(|name| name.rsplit_once(':'))
                {
                    let tag = tag.to_ascii_lowercase();
                    // Ollama can proxy these explicit cloud references even via a loopback URL.
                    ensure!(
                        tag != "cloud" && !tag.ends_with("-cloud"),
                        "正文向量须使用已安装的 Ollama 本地模型"
                    );
                }
                ensure!(
                    dimensions.is_none_or(|size| (1..=MAX_DIMENSIONS).contains(&size)),
                    "向量维度须在 1 至 {MAX_DIMENSIONS} 之间"
                );
                *endpoint = local_endpoint(endpoint)?;
                Some(
                    ureq::AgentBuilder::new()
                        .try_proxy_from_env(false)
                        .redirects(0)
                        .timeout_connect(Duration::from_secs(3))
                        .timeout_read(Duration::from_secs(45))
                        .timeout_write(Duration::from_secs(10))
                        .timeout(Duration::from_secs(55))
                        .build(),
                )
            }
        };
        Ok(Self { profile, agent })
    }

    pub fn profile(&self) -> &EmbeddingProfile {
        &self.profile
    }

    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        ensure!(
            texts.iter().all(|text| text.len() <= MAX_INPUT_BYTES),
            "向量输入超过大小限制"
        );
        match &self.profile {
            EmbeddingProfile::LocalLexicalV1 => {
                Ok(texts.iter().map(|text| lexical_vector(text)).collect())
            }
            EmbeddingProfile::OllamaV1 {
                model,
                endpoint,
                dimensions,
            } => self.embed_ollama(texts, model, endpoint, *dimensions),
        }
    }

    fn embed_ollama(
        &self,
        texts: &[&str],
        model: &str,
        endpoint: &str,
        mut dimensions: Option<usize>,
    ) -> Result<Vec<Vec<f32>>> {
        #[derive(Serialize)]
        struct Request<'a> {
            model: &'a str,
            input: &'a [&'a str],
            truncate: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            dimensions: Option<usize>,
        }

        #[derive(Deserialize)]
        struct Response {
            embeddings: Vec<Vec<f32>>,
        }

        let agent = self.agent.as_ref().context("Ollama 连接未初始化")?;
        let mut vectors = Vec::with_capacity(texts.len());
        let requested_dimensions = dimensions;
        for batch in texts.chunks(BATCH_SIZE) {
            let response = agent
                .post(endpoint)
                .send_json(Request {
                    model,
                    input: batch,
                    truncate: false,
                    dimensions: requested_dimensions,
                })
                .with_context(|| format!("Ollama 向量模型「{model}」请求失败"))?;
            ensure!(
                response.status() == 200,
                "Ollama 向量模型「{model}」返回 HTTP {}",
                response.status()
            );
            let bytes = read_limited(response.into_reader(), MAX_RESPONSE_BYTES)
                .context("无法读取 Ollama 向量响应")?;
            let response: Response =
                serde_json::from_slice(&bytes).context("Ollama 向量响应格式有误")?;
            ensure!(
                response.embeddings.len() == batch.len(),
                "Ollama 返回的向量数与输入数不一致"
            );
            for mut vector in response.embeddings {
                let expected = dimensions.get_or_insert(vector.len());
                ensure!(
                    (1..=MAX_DIMENSIONS).contains(&vector.len()) && vector.len() == *expected,
                    "Ollama 向量维度与预期不一致"
                );
                ensure!(normalize(&mut vector), "Ollama 返回了无效向量");
                vectors.push(vector);
            }
        }
        Ok(vectors)
    }
}

fn local_endpoint(endpoint: &str) -> Result<String> {
    let rest = endpoint
        .trim()
        .strip_prefix("http://")
        .context("Ollama 向量地址须使用本机 HTTP")?;
    let (authority, path) = rest
        .split_once('/')
        .context("Ollama 向量地址须包含 /api/embed")?;
    ensure!(path == "api/embed", "Ollama 向量地址须以 /api/embed 结尾");
    let (host, port) = authority
        .rsplit_once(':')
        .context("Ollama 地址须包含端口")?;
    let host = match host {
        "127.0.0.1" | "localhost" => "127.0.0.1",
        "[::1]" => "[::1]",
        _ => anyhow::bail!("Ollama 地址须使用 127.0.0.1、localhost 或 [::1]"),
    };
    let port: u16 = port.parse().context("Ollama 端口无效")?;
    ensure!(port > 0, "Ollama 端口无效");
    Ok(format!("http://{host}:{port}/api/embed"))
}

fn read_limited(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "内容超过大小限制");
    Ok(bytes)
}

fn lexical_vector(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0; LOCAL_DIMENSIONS];
    let mut word = String::new();
    let mut previous = None;
    let mut earlier = None;
    for ch in text.chars().flat_map(char::to_lowercase) {
        if is_cjk(ch) {
            add_word(&mut vector, &mut word);
            let mut buffer = [0; 12];
            let last = ch.encode_utf8(&mut buffer).len();
            add_feature(&mut vector, 1, &buffer[..last], 0.6);
            if let Some(prev) = previous {
                let last = encode_chars([prev, ch], &mut buffer);
                add_feature(&mut vector, 2, &buffer[..last], 1.0);
                if let Some(before) = earlier {
                    let last = encode_chars([before, prev, ch], &mut buffer);
                    add_feature(&mut vector, 3, &buffer[..last], 0.7);
                }
            }
            earlier = previous;
            previous = Some(ch);
        } else {
            previous = None;
            earlier = None;
            if ch.is_alphanumeric() || ch == '_' {
                word.push(ch);
            } else {
                add_word(&mut vector, &mut word);
            }
        }
    }
    add_word(&mut vector, &mut word);
    if !normalize(&mut vector) {
        // Punctuation-only input still gets a stable unit vector.
        add_feature(&mut vector, 4, text.trim().as_bytes(), 1.0);
    }
    vector
}

fn add_word(vector: &mut [f32], word: &mut String) {
    if !word.is_empty() {
        add_feature(vector, 0, word.as_bytes(), 1.0);
        word.clear();
    }
}

fn encode_chars<const N: usize>(chars: [char; N], buffer: &mut [u8; 12]) -> usize {
    let mut len = 0;
    for ch in chars {
        len += ch.encode_utf8(&mut buffer[len..]).len();
    }
    len
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3041}'..='\u{3096}' | '\u{30A1}'..='\u{30FA}' | '\u{3400}'..='\u{4DBF}' |
        '\u{4E00}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{F900}'..='\u{FAFF}' |
        '\u{20000}'..='\u{2FA1F}' | '\u{30000}'..='\u{323AF}'
    )
}

fn add_feature(vector: &mut [f32], kind: u8, value: &[u8], weight: f32) {
    // FNV-1a keeps an on-disk index reproducible across processes and Rust releases.
    let mut hash = (0xcbf29ce484222325_u64 ^ u64::from(kind)).wrapping_mul(0x100000001b3);
    for byte in value {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    let index = (hash % vector.len() as u64) as usize;
    vector[index] += if hash >> 63 == 0 { weight } else { -weight };
}

fn normalize(vector: &mut [f32]) -> bool {
    let norm = vector
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm == 0.0 || !norm.is_finite() {
        return false;
    }
    for value in vector {
        *value = (f64::from(*value) / norm) as f32;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn loads_embedding_profile_from_settings_json() -> Result<()> {
        let folder = tempfile::tempdir()?;
        fs::write(
            folder.path().join("settings.json"),
            r#"{"embedding":{"model":"bge-m3","endpoint":"http://127.0.0.1:11434/api/embed","dimensions":1024}}"#,
        )?;
        let provider = Embeddings::load(folder.path())?;
        assert_eq!(
            provider.profile(),
            &EmbeddingProfile::OllamaV1 {
                model: "bge-m3".into(),
                endpoint: "http://127.0.0.1:11434/api/embed".into(),
                dimensions: Some(1024),
            }
        );
        Ok(())
    }

    #[test]
    fn local_vectors_rank_shared_chinese_terms() -> Result<()> {
        let provider = Embeddings::from_profile(EmbeddingProfile::LocalLexicalV1)?;
        let vectors = provider.embed(&[
            "重置密码",
            "请使用账号中心重置密码，然后重新登录。",
            "天气预报预计今天多云，明天可能下雨。",
            "?!",
            " ",
        ])?;
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(vectors.iter().all(|v| (dot(v, v) - 1.0).abs() < 0.000_01));
        assert_eq!(vectors[0].len(), LOCAL_DIMENSIONS);
        assert!(dot(&vectors[0], &vectors[1]) > 0.3);
        assert!(dot(&vectors[0], &vectors[1]) > dot(&vectors[0], &vectors[2]));
        assert_eq!(vectors[0], provider.embed(&["重置密码"])?[0]);
        assert_eq!(provider.embed(&["MOIN"])?[0], provider.embed(&["moin"])?[0]);
        Ok(())
    }

    #[test]
    fn pins_validated_loopback_profile() -> Result<()> {
        let endpoint = local_endpoint(" http://localhost:11434/api/embed ")?;
        assert_eq!(endpoint, EmbeddingConfig::default().endpoint);
        assert!(local_endpoint("http://192.0.2.1:11434/api/embed").is_err());
        assert!(local_endpoint("http://127.0.0.1:11434/api/embed?redirect=1").is_err());
        let profile = EmbeddingProfile::OllamaV1 {
            model: "bge-m3".into(),
            endpoint,
            dimensions: Some(1024),
        };
        let decoded = serde_json::from_str(&serde_json::to_string(&profile)?)?;
        assert_eq!(Embeddings::from_profile(decoded)?.profile(), &profile);
        for model in ["bge-m3:cloud", "bge-m3:latest-CLOUD"] {
            assert!(
                Embeddings::from_profile(EmbeddingProfile::OllamaV1 {
                    model: model.into(),
                    endpoint: EmbeddingConfig::default().endpoint,
                    dimensions: None,
                })
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn ollama_posts_batched_unicode_and_normalizes_response() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = format!("http://{}/api/embed", listener.local_addr()?);
        let server = thread::spawn(move || -> Result<()> {
            let (stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut request = BufReader::new(stream);
            let mut first_line = String::new();
            request.read_line(&mut first_line)?;
            assert!(first_line.starts_with("POST /api/embed "));
            let mut size = None;
            loop {
                let mut header = String::new();
                if request.read_line(&mut header)? == 0 || header == "\r\n" {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("Content-Length")
                {
                    size = Some(value.trim().parse::<usize>()?);
                }
            }
            let mut body = vec![0; size.context("缺少 Content-Length")?];
            request.read_exact(&mut body)?;
            let sent: serde_json::Value = serde_json::from_slice(&body)?;
            assert_eq!(sent["model"], "bge-m3");
            assert_eq!(sent["input"], serde_json::json!(["重置密码", "第二段"]));
            assert_eq!(sent["dimensions"], 2);
            assert_eq!(sent["truncate"], false);
            let body = r#"{"embeddings":[[3.0,4.0],[0.0,2.0]]}"#;
            write!(
                request.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
            Ok(())
        });
        let provider = Embeddings::from_profile(EmbeddingProfile::OllamaV1 {
            model: "bge-m3".into(),
            endpoint,
            dimensions: Some(2),
        })?;
        let result = provider.embed(&["重置密码", "第二段"]);
        server.join().expect("模拟 Ollama 服务崩溃")?;
        assert_eq!(result?, vec![vec![0.6, 0.8], vec![0.0, 1.0]]);
        Ok(())
    }
}
