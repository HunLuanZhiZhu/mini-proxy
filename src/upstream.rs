// 上游请求客户端与流式透传

use crate::config::{Endpoint, KeyMode, PathMode};
use crate::protocol::Protocol;
use anyhow::Result;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode};
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use reqwest::Client;
use serde_json::Value;
use std::str::FromStr;
use std::time::Duration;

// OpenCode Go API 要求所有请求携带的会话头（用于会话路由 + prompt cache 亲和）
pub const SESSION_HEADER: &str = "x-opencode-session";

// 从请求体推导会话标识：
//   Anthropic: metadata.user_id（claude.exe 格式为 user_<hash>_account_<uuid>_session_<uuid>，
//              取 _session_ 后缀 → 每个对话一个稳定 ID）
//   OpenAI/Responses: 顶层 user 字段
// 字段缺失 / 非合法 JSON / 空串 → None，由调用方回退到 [provider.headers] 静态配置
fn session_header_value(protocol: Protocol, body: &Bytes) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let raw = match protocol {
        Protocol::Anthropic => v
            .get("metadata")
            .and_then(|m| m.get("user_id"))
            .and_then(|u| u.as_str())?,
        _ => v.get("user").and_then(|u| u.as_str())?,
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Some(match raw.rsplit_once("_session_") {
        Some((_, s)) if !s.is_empty() => s.to_string(),
        _ => raw.to_string(),
    })
}

pub struct UpstreamClient {
    http: Client,
    // x-opencode-session 的兜底值：每次进程启动随机生成（无需用户配置）。
    // 请求体推导不出会话 ID 时使用；同一进程内所有请求共用，重启后变化
    session_fallback: String,
}

impl UpstreamClient {
    pub fn new() -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(1800))
            .build()
            .expect("构建 HTTP 客户端失败");
        Self {
            http,
            session_fallback: uuid::Uuid::now_v7().to_string(),
        }
    }

    fn upstream_url(&self, ep: &Endpoint, protocol: Protocol) -> String {
        let base = ep.base_url.trim_end_matches('/');
        match ep.path_mode {
            PathMode::Append => format!("{}{}", base, protocol.append_suffix()),
            PathMode::Full => ep.base_url.clone(),
        }
    }

    pub async fn send(
        &self,
        ep: &Endpoint,
        protocol: Protocol,
        body: &Bytes,
        client_headers: &HeaderMap,
    ) -> Result<UpstreamResponse> {
        let url = self.upstream_url(ep, protocol);
        let mut req = self.http.request(Method::POST, &url);

        let use_override = matches!(ep.key_mode, KeyMode::Override) && !ep.api_key.is_empty();
        if use_override {
            req = match protocol {
                Protocol::OpenAI | Protocol::Responses => req.bearer_auth(&ep.api_key),
                Protocol::Anthropic => req
                    .header("x-api-key", &ep.api_key)
                    .header("anthropic-version", "2023-06-01"),
            };
        } else {
            let mut has_anthropic_version = false;
            for (name, value) in client_headers.iter() {
                let name_lower = name.as_str().to_lowercase();
                if name_lower == "authorization" || name_lower == "x-api-key" {
                    req = req.header(name, value);
                }
                if name_lower == "anthropic-version" {
                    has_anthropic_version = true;
                }
            }
            if protocol == Protocol::Anthropic && !has_anthropic_version {
                req = req.header("anthropic-version", "2023-06-01");
            }
        }

        for (name, value) in client_headers.iter() {
            let name_lower = name.as_str().to_lowercase();
            if matches!(
                name_lower.as_str(),
                "authorization" | "x-api-key" | "anthropic-version" | "host"
                    | "content-length" | "connection" | "transfer-encoding"
            ) {
                continue;
            }
            req = req.header(name, value);
        }

        req = req.header("content-type", "application/json");

        // x-opencode-session：仅 is_opencode = true 的供应商注入；客户端没带才补。
        // 取值顺序：请求体推导 > [provider.headers] 静态配置 > 启动时随机生成的进程级兜底值
        if ep.is_opencode && !client_headers.contains_key(SESSION_HEADER) {
            let (source, value) = match session_header_value(protocol, body) {
                Some(s) => ("请求体推导", s),
                None => match ep.headers.get(SESSION_HEADER) {
                    Some(v) => ("静态配置", v.clone()),
                    None => ("启动随机值", self.session_fallback.clone()),
                },
            };
            tracing::info!(session = %value, source, "已注入会话头");
            if let Ok(hv) = HeaderValue::from_str(&value) {
                req = req.header(SESSION_HEADER, hv);
            }
        }
        // 其余静态注入头：同样只补客户端没带的键
        for (k, v) in &ep.headers {
            if k.eq_ignore_ascii_case(SESSION_HEADER) || client_headers.contains_key(k.as_str()) {
                continue;
            }
            if let (Ok(hk), Ok(hv)) = (HeaderName::from_str(k), HeaderValue::from_str(v)) {
                req = req.header(hk, hv);
            }
        }

        let resp = req.body(body.clone()).send().await?;

        let status = StatusCode::from_u16(resp.status().as_u16())?;
        let is_stream = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.contains("text/event-stream"))
            .unwrap_or(false);

        let upstream_headers = resp.headers().clone();

        Ok(UpstreamResponse {
            status,
            is_stream,
            headers: upstream_headers,
            resp: Some(resp),
            body_bytes: None,
            preloaded: None,
        })
    }

    // 辅助路径透传：方法/路径/查询串/body 原样转发，目标 = base_url + 原路径
    // 不改 body、不做模型映射、不注入思考强度、不重试；鉴权按 key_mode 处理
    pub async fn send_aux(
        &self,
        ep: &Endpoint,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: &Bytes,
        client_headers: &HeaderMap,
    ) -> Result<UpstreamResponse> {
        let base = ep.base_url.trim_end_matches('/');
        let mut url = format!("{}/{}", base, path.trim_start_matches('/'));
        if let Some(q) = query.filter(|q| !q.is_empty()) {
            url.push('?');
            url.push_str(q);
        }

        let use_override = matches!(ep.key_mode, KeyMode::Override) && !ep.api_key.is_empty();
        let mut req = self.http.request(method, &url);
        if use_override {
            // 辅助端点格式未知，两种鉴权头都带上（网关只会认其中一种）
            req = req
                .header("authorization", format!("Bearer {}", ep.api_key))
                .header("x-api-key", &ep.api_key);
        }

        let mut has_anthropic_version = false;
        for (name, value) in client_headers.iter() {
            let name_lower = name.as_str().to_lowercase();
            match name_lower.as_str() {
                "authorization" | "x-api-key" => {
                    if !use_override {
                        req = req.header(name, value);
                    }
                }
                // 逐跳头 / host / 长度：由本段连接自己决定
                "host" | "content-length" | "connection" | "transfer-encoding" => {}
                _ => {
                    if name_lower == "anthropic-version" {
                        has_anthropic_version = true;
                    }
                    req = req.header(name, value);
                }
            }
        }
        // Anthropic 格式的辅助端点（如 /v1/models）要求 anthropic-version
        if !has_anthropic_version && client_headers.contains_key("x-api-key") {
            req = req.header("anthropic-version", "2023-06-01");
        }
        // x-opencode-session：仅 is_opencode = true 的供应商注入；辅助路径 body 格式
        // 未知不做请求体推导，客户端没带才补，取静态配置或启动随机值
        if ep.is_opencode && !client_headers.contains_key(SESSION_HEADER) {
            let (source, value) = match ep.headers.get(SESSION_HEADER) {
                Some(v) => ("静态配置", v.clone()),
                None => ("启动随机值", self.session_fallback.clone()),
            };
            tracing::info!(source, "辅助路径已注入会话头");
            if let Ok(hv) = HeaderValue::from_str(&value) {
                req = req.header(SESSION_HEADER, hv);
            }
        }
        // 静态注入头：只补客户端没带的键（辅助路径 body 格式未知，不做会话推导）
        for (k, v) in &ep.headers {
            if client_headers.contains_key(k.as_str()) {
                continue;
            }
            if let (Ok(hk), Ok(hv)) = (HeaderName::from_str(k), HeaderValue::from_str(v)) {
                req = req.header(hk, hv);
            }
        }

        let resp = req.body(body.clone()).send().await?;

        let status = StatusCode::from_u16(resp.status().as_u16())?;
        let is_stream = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.contains("text/event-stream"))
            .unwrap_or(false);
        let upstream_headers = resp.headers().clone();

        Ok(UpstreamResponse {
            status,
            is_stream,
            headers: upstream_headers,
            resp: Some(resp),
            body_bytes: None,
            preloaded: None,
        })
    }
}

pub struct UpstreamResponse {
    pub status: StatusCode,
    pub is_stream: bool,
    pub headers: HeaderMap,
    pub resp: Option<reqwest::Response>,
    // 非流式：完整 body
    pub body_bytes: Option<Bytes>,
    // 流式：预读的 chunks + 剩余 stream
    pub preloaded: Option<(Vec<Bytes>, Box<dyn futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin>)>,
}

impl UpstreamResponse {
    // 非流式：读完整 body
    // 流式：读前几个 chunk 判断是否含 error，保留剩余 stream
    pub async fn preload_body(&mut self) {
        // 非流式
        if self.body_bytes.is_none() && !self.is_stream {
            if let Some(resp) = self.resp.take() {
                self.body_bytes = Some(
                    resp.bytes()
                        .await
                        .unwrap_or_else(|_| Bytes::new()),
                );
            }
            return;
        }

        // 流式：读前几个 chunk，保留剩余 stream
        if self.preloaded.is_none() && self.is_stream {
            if let Some(resp) = self.resp.take() {
                let mut stream = resp.bytes_stream();
                let mut chunks: Vec<Bytes> = Vec::new();
                let mut buf = String::new();

                // 最多读 16 个 chunk，用于判断是否含 error
                for _ in 0..16 {
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            buf.push_str(&String::from_utf8_lossy(&chunk));
                            chunks.push(chunk);
                            // 遇到 error 事件 → 停止（是错误，可重试）
                            if buf.contains("event: error")
                                && buf.contains("\"error\":")
                                && buf.contains("\"code\":")
                            {
                                break;
                            }
                            // 遇到有效内容事件 → 停止（不是错误）
                            if buf.contains("response.output_text")
                                || buf.contains("response.completed")
                                || buf.contains("response.output_item")
                                || buf.contains("content_block_delta")
                                || buf.contains("chat.completion.chunk")
                            {
                                break;
                            }
                        }
                        _ => break,
                    }
                }

                // 保留剩余 stream（用于转发时拼合）
                let remaining: Box<dyn futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin> =
                    Box::new(stream);
                self.preloaded = Some((chunks, remaining));
            }
        }
    }

    // 从已读内容中解析业务错误码
    pub fn extract_error_code(&self) -> Option<i64> {
        // 非流式
        if let Some(bytes) = &self.body_bytes {
            let val: serde_json::Value = serde_json::from_slice(bytes).ok()?;
            let code = val.get("error")?.get("code")?;
            if let Some(n) = code.as_i64() {
                return Some(n);
            }
            if let Some(s) = code.as_str() {
                return s.parse::<i64>().ok();
            }
            return None;
        }

        // 流式：从预读 chunks 拼接后查找
        if let Some((chunks, _)) = &self.preloaded {
            let text: String = chunks
                .iter()
                .map(|c| String::from_utf8_lossy(c).to_string())
                .collect::<String>();

            for line in text.lines() {
                if line.starts_with("data:") {
                    let json_str = line.trim_start_matches("data:").trim();
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(code) = val.get("error").and_then(|e| e.get("code")) {
                            if let Some(n) = code.as_i64() {
                                return Some(n);
                            }
                            if let Some(s) = code.as_str() {
                                return s.parse::<i64>().ok();
                            }
                        }
                    }
                }
            }
        }

        None
    }

    pub async fn into_axum(self) -> Response<Body> {
        let mut builder = Response::builder().status(self.status);

        for (name, value) in self.headers.iter() {
            let name_lower = name.as_str().to_lowercase();
            // body 原样转发，content-encoding 必须跟着转发，否则压缩体会被客户端当明文解析；
            // 代价：上游压缩时 preload_body / extract_error_code 读不到明文，业务错误码重试失效
            if matches!(
                name_lower.as_str(),
                "content-length" | "connection" | "transfer-encoding"
            ) {
                continue;
            }
            if let Ok(name) = HeaderName::from_bytes(name.as_ref()) {
                if let Ok(value) = HeaderValue::from_bytes(value.as_bytes()) {
                    builder = builder.header(name, value);
                }
            }
        }

        // 非流式
        if !self.is_stream {
            if let Some(bytes) = self.body_bytes {
                return builder.body(Body::from(bytes)).unwrap();
            }
            if let Some(resp) = self.resp {
                let bytes = resp.bytes().await.unwrap_or_else(|_| Bytes::new());
                return builder.body(Body::from(bytes)).unwrap();
            }
            return builder.body(Body::empty()).unwrap();
        }

        // 流式：预读 chunks + 剩余 stream 拼合成完整流
        if let Some((chunks, remaining)) = self.preloaded {
            // 把已读 chunks 作为即时流，剩余 stream 接在后面
            let chunk_stream = stream::iter(chunks.into_iter().map(Ok::<Bytes, std::io::Error>));
            let combined = chunk_stream.chain(remaining.map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))));
            let body = Body::from_stream(combined);
            return builder.body(body).unwrap();
        }

        // 未预读的流式：直接流式转发
        if let Some(resp) = self.resp {
            let stream = resp.bytes_stream();
            let body = Body::from_stream(stream);
            return builder.body(body).unwrap();
        }

        builder.body(Body::empty()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic_body(user_id: &str) -> Bytes {
        Bytes::from(format!(
            r#"{{"model":"claude-opus-5-5","metadata":{{"user_id":"{}"}}}}"#,
            user_id
        ))
    }

    #[test]
    fn anthropic_user_id_takes_session_suffix() {
        let body = anthropic_body(
            "user_ab12cd34_account_11111111-2222-3333-4444-555555555555_session_99999999-8888-7777-6666-555555555555",
        );
        assert_eq!(
            session_header_value(Protocol::Anthropic, &body).as_deref(),
            Some("99999999-8888-7777-6666-555555555555")
        );
    }

    #[test]
    fn anthropic_user_id_without_session_part_falls_back_to_whole() {
        let body = anthropic_body("user_ab12cd34");
        assert_eq!(
            session_header_value(Protocol::Anthropic, &body).as_deref(),
            Some("user_ab12cd34")
        );
    }

    #[test]
    fn openai_takes_user_field() {
        let body = Bytes::from(r#"{"model":"gpt-x","user":"user-98765"}"#);
        assert_eq!(
            session_header_value(Protocol::OpenAI, &body).as_deref(),
            Some("user-98765")
        );
        // Responses 协议同 OpenAI
        assert_eq!(
            session_header_value(Protocol::Responses, &body).as_deref(),
            Some("user-98765")
        );
    }

    #[test]
    fn missing_or_invalid_yields_none() {
        // 没有 metadata / user 字段
        assert_eq!(
            session_header_value(Protocol::Anthropic, &Bytes::from(r#"{"model":"m"}"#)),
            None
        );
        // metadata 里没有 user_id
        assert_eq!(
            session_header_value(
                Protocol::Anthropic,
                &Bytes::from(r#"{"metadata":{"other":1}}"#)
            ),
            None
        );
        // 非 JSON body
        assert_eq!(
            session_header_value(Protocol::OpenAI, &Bytes::from_static(b"not json")),
            None
        );
        // 空串
        assert_eq!(
            session_header_value(Protocol::OpenAI, &Bytes::from(r#"{"user":""}"#)),
            None
        );
    }
}
