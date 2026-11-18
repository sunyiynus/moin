use extism_pdk::{config, plugin_fn, Error, FnResult, Json};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct Request {
    version: u32,
    input: String,
}

#[derive(Serialize)]
pub struct Response {
    title: String,
    detail: String,
    output: String,
}

#[plugin_fn]
pub fn run(Json(request): Json<Request>) -> FnResult<Json<Response>> {
    if request.version != 1 {
        return Err(Error::msg("不支持的 Moin 插件协议版本").into());
    }
    let name = request.input.trim();
    let name = if name.is_empty() { "世界" } else { name };
    let greeting = config::get("greeting")?.unwrap_or_else(|| "你好".into());
    Ok(Json(Response {
        title: "问候".into(),
        detail: "Extism · Rust / WebAssembly".into(),
        output: format!("{greeting}，{name}！"),
    }))
}
