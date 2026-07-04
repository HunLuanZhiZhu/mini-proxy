// 上游请求客户端与流式透传

use crate::config::{Endpoint, KeyMode, PathMode};
use crate::protocol::Protocol;
use anyhow::Result;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode};
use bytes::Bytes;
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
                Protocol::OpenAI => req.bearer_auth(&ep.api_key),
                Protocol::Claude => req
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
            if protocol == Protocol::Claude && !has_anthropic_version {
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
        })
    }
}

pub struct UpstreamResponse {
    pub status: StatusCode,
    pub is_stream: bool,
    pub headers: HeaderMap,
    pub resp: Option<reqwest::Response>,
    pub body_bytes: Option<Bytes>,
}

impl UpstreamResponse {
    // 非流式时预读 body 存入 body_bytes，供业务码判断
    pub async fn preload_body(&mut self) {
        if self.body_bytes.is_none() && !self.is_stream {
            if let Some(resp) = self.resp.take() {
                self.body_bytes = Some(
                    resp.bytes()
                        .await
                        .unwrap_or_else(|_| Bytes::new()),
                );
            }
        }
    }

    // 从已读 body 中解析业务错误码（error.code 字段）
    // OpenAI 格式：{"error":{"code":11210,"message":"tpm超限"}}
    // Anthropic 格式：{"error":{"type":"...","message":"..."}}
    pub fn extract_error_code(&self) -> Option<i64> {
        let bytes = self.body_bytes.as_ref()?;
        let val: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let code = val.get("error")?.get("code")?;
        if let Some(n) = code.as_i64() {
            return Some(n);
        }
        if let Some(s) = code.as_str() {
            return s.parse::<i64>().ok();
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

        if self.is_stream {
            let resp = self.resp.expect("流式响应已被消费");
            let stream = resp.bytes_stream();
            let body = Body::from_stream(stream);
            builder.body(body).unwrap()
        } else if let Some(bytes) = self.body_bytes {
            builder.body(Body::from(bytes)).unwrap()
        } else if let Some(resp) = self.resp {
            let bytes = resp.bytes().await.unwrap_or_else(|_| Bytes::new());
            builder.body(Body::from(bytes)).unwrap()
        } else {
            builder.body(Body::empty()).unwrap()
        }
    }
}
