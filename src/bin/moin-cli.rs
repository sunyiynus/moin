use anyhow::{Context, Result, bail};
use moin::{
    config,
    search::index::{self, MAX_QUERY_CHARS},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

const USAGE: &str = "moin-cli search <查询> [--limit <条数>]
moin-cli status
moin-cli reindex
moin-cli mcp";
const DEFAULT_LIMIT: usize = 8;
const MAX_LIMIT: usize = 30;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const TOOL: &str = "search_local_documents";
const MODERN_VERSION: &str = "2026-07-28";
const LEGACY_VERSIONS: [&str; 2] = ["2025-11-25", "2025-06-18"];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    let args = args.collect::<Vec<_>>();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    match (command.as_deref(), args.as_slice()) {
        (None | Some("--help" | "-h" | "help"), []) => writeln!(output, "{USAGE}")?,
        (Some("search"), [query]) => cli_search(query, DEFAULT_LIMIT, &mut output)?,
        (Some("search"), [query, flag, limit]) if flag == "--limit" => {
            let limit = limit.parse().context("--limit 需要整数")?;
            cli_search(query, limit, &mut output)?;
        }
        (Some("mcp"), []) => {
            let database = config::data_dir()?.join("index.sqlite3");
            serve_mcp(&database, io::stdin().lock(), &mut output)?;
        }
        (Some("status" | "reindex"), []) => {
            let home = config::data_dir()?;
            let database = home.join("index.sqlite3");
            let settings = config::Config::load(&home)?;
            let summary = if command.as_deref() == Some("reindex") {
                index::rebuild(&database, &settings.roots)?
            } else {
                index::summary(&database)?
            };
            write_json(
                &mut output,
                &json!({
                    "database": database,
                    "roots": settings.roots,
                    "apps": summary.apps,
                    "files": summary.files,
                    "chunks": summary.chunks,
                    "vector_database": index::vector_directory(&database),
                    "vector_backend": "LanceDB",
                    "vectors": summary.vectors,
                    "embedding": summary.embedding,
                    "semantic": summary.semantic,
                }),
            )?;
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}

fn cli_search(query: &str, limit: usize, output: &mut impl Write) -> Result<()> {
    let database = config::data_dir()?.join("index.sqlite3");
    write_json(output, &passages(&database, query, limit)?)
}

#[derive(Serialize)]
struct Passage {
    title: String,
    path: PathBuf,
    chunk: usize,
    text: String,
}

fn passages(database: &Path, query: &str, limit: usize) -> Result<Vec<Passage>> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
        bail!("查询长度应为 1 到 {MAX_QUERY_CHARS} 个字符");
    }
    if !(1..=MAX_LIMIT).contains(&limit) {
        bail!("结果条数应为 1 到 {MAX_LIMIT}");
    }
    Ok(index::retrieve(database, query, limit)?
        .into_iter()
        .filter_map(|hit| {
            Some(Passage {
                title: hit.title,
                path: hit.path,
                chunk: hit.chunk?,
                text: hit.excerpt,
            })
        })
        .collect())
}

fn write_json(output: &mut impl Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn serve_mcp(database: &Path, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    use std::io::Read as _;

    let mut server = McpServer {
        database,
        legacy_initialized: false,
    };
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut input)
            .take((MAX_MESSAGE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        let reply = if read > MAX_MESSAGE_BYTES {
            if !line.ends_with(b"\n") {
                input.skip_until(b'\n')?;
            }
            Some(rpc_error(
                Value::Null,
                -32600,
                "JSON-RPC message exceeds 64 KiB",
            ))
        } else {
            let message = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&line);
            match serde_json::from_slice(message) {
                Ok(message) => server.handle(&message),
                Err(_) => Some(rpc_error(Value::Null, -32700, "Parse error")),
            }
        };
        if let Some(reply) = reply {
            write_json(&mut output, &reply)?;
        }
    }
}

struct McpServer<'a> {
    database: &'a Path,
    legacy_initialized: bool,
}

impl McpServer<'_> {
    fn handle(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id");
        let method = message["method"].as_str();
        let valid_id = id.is_none_or(|value| {
            value.is_string() || value.as_i64().is_some() || value.as_u64().is_some()
        });
        if message["jsonrpc"] != "2.0" || method.is_none() || !valid_id {
            return Some(rpc_error(
                id.filter(|_| valid_id).cloned().unwrap_or(Value::Null),
                -32600,
                "Invalid Request",
            ));
        }
        // Notifications, including notifications/initialized, have no response.
        let id = id?.clone();
        let method = method.unwrap_or_default();
        let params = &message["params"];
        if message.get("params").is_some() && !params.is_object() {
            return Some(rpc_error(id, -32602, "Expected an object in params"));
        }

        let meta = &params["_meta"];
        let modern = method == "server/discover" || meta.get(VERSION_KEY).is_some();
        if modern {
            let (Some(version), true) = (
                meta[VERSION_KEY].as_str(),
                meta[CAPABILITIES_KEY].is_object(),
            ) else {
                return Some(rpc_error(
                    id,
                    -32602,
                    "MCP protocolVersion and clientCapabilities are required in _meta",
                ));
            };
            if version != MODERN_VERSION {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32022,
                        "message": "Unsupported protocol version",
                        "data": { "supported": [MODERN_VERSION], "requested": version }
                    }
                }));
            }
        }

        let mut result = match method {
            "initialize" if !modern => {
                let Some(requested) = params["protocolVersion"].as_str() else {
                    return Some(rpc_error(id, -32602, "Missing protocolVersion"));
                };
                let version = if LEGACY_VERSIONS.contains(&requested) {
                    requested
                } else {
                    LEGACY_VERSIONS[0]
                };
                self.legacy_initialized = true;
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": server_info()
                })
            }
            "server/discover" => json!({
                "supportedVersions": [MODERN_VERSION],
                "capabilities": { "tools": {} }
            }),
            "ping" => json!({}),
            "tools/list" | "tools/call" if !modern && !self.legacy_initialized => {
                return Some(rpc_error(id, -32600, "MCP initialization required"));
            }
            "tools/list" if params.get("cursor").is_some() => {
                return Some(rpc_error(id, -32602, "Invalid pagination cursor"));
            }
            "tools/list" => json!({ "tools": [tool_definition()] }),
            "tools/call" => {
                if params["name"].as_str() != Some(TOOL) {
                    return Some(rpc_error(id, -32602, "Unknown tool"));
                }
                call_search_tool(self.database, &params["arguments"])
            }
            _ => return Some(rpc_error(id, -32601, "Method not found")),
        };
        if modern {
            result["resultType"] = json!("complete");
            result["_meta"] = json!({ (SERVER_INFO_KEY): server_info() });
        }
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn server_info() -> Value {
    json!({ "name": "moin", "version": env!("CARGO_PKG_VERSION") })
}

fn tool_definition() -> Value {
    json!({
        "name": TOOL,
        "title": "Search Moin local documents",
        "description": "Retrieve text passages from Moin's local document index. Cite the path and one-based chunk number for each passage.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1, "maxLength": MAX_QUERY_CHARS },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "default": DEFAULT_LIMIT }
            },
            "required": ["query"],
            "additionalProperties": false
        },
        "outputSchema": {
            "type": "object",
            "properties": {
                "passages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "path": { "type": "string" },
                            "chunk": { "type": "integer", "minimum": 1 },
                            "text": { "type": "string" }
                        },
                        "required": ["title", "path", "chunk", "text"]
                    }
                }
            },
            "required": ["passages"]
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": false }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

fn call_search_tool(database: &Path, args: &Value) -> Value {
    let result = serde_json::from_value::<SearchArguments>(args.clone())
        .context("需要查询文本 query 和可选的结果条数 limit")
        .and_then(|args| passages(database, &args.query, args.limit));
    match result {
        Ok(passages) => {
            let structured = json!({ "passages": passages });
            json!({
                "content": [{ "type": "text", "text": structured.to_string() }],
                "structuredContent": structured
            })
        }
        Err(error) => json!({
            "content": [{ "type": "text", "text": format!("{error:#}") }],
            "isError": true
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_lists_and_reads_existing_passages_in_both_protocol_eras() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let database = temp.path().join("index.sqlite3");
        let notes = temp.path().join("notes.md");
        std::fs::write(&notes, "项目预算将在周五审议。")?;
        index::rebuild(&database, &[notes])?;
        let requests = [
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": LEGACY_VERSIONS[1], "clientInfo": {"name":"test","version":"1"}, "capabilities": {} }
            }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": TOOL, "arguments": { "query": "项目预算" } }
            }),
            json!({
                "jsonrpc": "2.0", "id": 4, "method": "server/discover",
                "params": { "_meta": { (VERSION_KEY): MODERN_VERSION, (CAPABILITIES_KEY): {} } }
            }),
            json!({
                "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": {
                    "_meta": { (VERSION_KEY): MODERN_VERSION, (CAPABILITIES_KEY): {} },
                    "name": TOOL, "arguments": { "query": "项目预算" }
                }
            }),
        ];
        let mut input = vec![0xef, 0xbb, 0xbf];
        for request in requests {
            write_json(&mut input, &request)?;
        }
        let mut output = Vec::new();
        serve_mcp(&database, input.as_slice(), &mut output)?;
        let responses = output
            .as_slice()
            .lines()
            .map(|line| serde_json::from_str::<Value>(&line?).map_err(io::Error::other))
            .collect::<io::Result<Vec<_>>>()?;
        assert_eq!(responses.len(), 5);
        assert_eq!(
            responses[0]["result"]["protocolVersion"],
            LEGACY_VERSIONS[1]
        );
        assert_eq!(responses[1]["result"]["tools"][0]["name"], TOOL);
        assert_eq!(
            responses[2]["result"]["structuredContent"]["passages"][0]["chunk"],
            1
        );
        assert_eq!(
            responses[3]["result"]["supportedVersions"][0],
            MODERN_VERSION
        );
        assert_eq!(responses[4]["result"]["resultType"], "complete");
        assert_eq!(
            responses[4]["result"]["structuredContent"]["passages"][0]["text"],
            "项目预算将在周五审议。"
        );
        Ok(())
    }
}
