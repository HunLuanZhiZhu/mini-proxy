// 上游请求客户端与流式透传

use crate::config::{Endpoint, KeyMode, PathMode};
use crate::protocol::Protocol;
use anyhow::Result;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode};
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::Client;
use std::time::Duration;

pub struct UpstreamClient {
    http: Client,
}

impl UpstreamClient {
    pub fn new() -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("构建 HTTP 客户端失败");
        Self { http }
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
            preloaded_chunks: None,
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
    // 流式：预读的 chunk 列表 + 剩余流
    pub preloaded_chunks: Option<(Vec<Bytes>, Option<reqwest::Response>)>,
}

impl UpstreamResponse {
    // 非流式：读完整 body
    // 流式：读前几个 chunk 累积到能判断是否含 error 为止
    pub async fn preload_body(&mut self) {
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

        // 流式：读前几个 chunk
        if self.preloaded_chunks.is_none() && self.is_stream {
            if let Some(resp) = self.resp.take() {
                let mut chunks: Vec<Bytes> = Vec::new();
                let mut stream = resp.bytes_stream();
                let mut buf = String::new();

                // 最多读 16 个 chunk 或 64KB，用于判断是否含 error
                for _ in 0..16 {
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            buf.push_str(&String::from_utf8_lossy(&chunk));
                            chunks.push(chunk);
                            // 检查是否已遇到 event: error 的 data 行
                            if buf.contains("event: error")
                                && buf.contains("\"error\":")
                                && buf.contains("\"code\":")
                            {
                                break;
                            }
                            // 如果遇到有效内容事件（说明不是错误），也停止预读
                            if buf.contains("response.output_text")
                                || buf.contains("response.completed")
                                || buf.contains("response.output_item")
                            {
                                break;
                            }
                        }
                        _ => break,
                    }
                }

                // 把剩余流存回去
                // reqwest 的 bytes_stream 消费了 resp，无法还原
                // 所以我们把已读 chunks 和「流已结束」标记存起来
                self.preloaded_chunks = Some((chunks, None));
            }
        }
    }

    // 从已读内容中解析业务错误码
    // 非流式：JSON 的 error.code 字段
    // 流式：SSE 中 event: error 后的 data 里 error.code 字段
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
        if let Some((chunks, _)) = &self.preloaded_chunks {
            let text: String = chunks
                .iter()
                .map(|c| String::from_utf8_lossy(c).to_string())
                .collect::<String>();

            // 查找所有 data: 行，解析 JSON，找 error.code
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
            if matches!(
                name_lower.as_str(),
                "content-length" | "connection" | "transfer-encoding" | "content-encoding"
            ) {
                continue;
            }
            if let Ok(name) = HeaderName::from_bytes(name.as_ref()) {
                if let Ok(value) = HeaderValue::from_bytes(value.as_bytes()) {
                    builder = builder.header(name, value);
                }
            }
        }

        // 非流式：用 body_bytes 或现读
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

        // 流式：用预读 chunks 拼接后整体返回
        if let Some((chunks, _remaining)) = self.preloaded_chunks {
            let total_len: usize = chunks.iter().map(|b| b.len()).sum();
            let mut out = bytes::BytesMut::with_capacity(total_len);
            for b in chunks {
                out.extend_from_slice(&b);
            }
            return builder.body(Body::from(out.freeze())).unwrap();
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

