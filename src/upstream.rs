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

    // 拼接上游 URL：path_mode = append 时按协议补后缀，full 时原样使用
    fn upstream_url(&self, ep: &Endpoint, protocol: Protocol) -> String {
        let base = ep.base_url.trim_end_matches('/');
        match ep.path_mode {
            PathMode::Append => format!("{}{}", base, protocol.append_suffix()),
            PathMode::Full => ep.base_url.clone(),
        }
    }

    // 发送一次请求，返回响应。
    // body 内的 model 字段已被调用方替换为上游模型名
    pub async fn send(
        &self,
        ep: &Endpoint,
        protocol: Protocol,
        body: &Bytes,
        client_headers: &HeaderMap,
    ) -> Result<UpstreamResponse> {
        let url = self.upstream_url(ep, protocol);
        let mut req = self.http.request(Method::POST, &url);

        // 鉴权头处理：
        //   override + api_key 非空：用 config 的 api_key 覆盖客户端 Key
        //   override + api_key 为空：回退到 passthrough 行为
        //   passthrough：透传客户端原 Key，config 不存储不管理
        let use_override = matches!(ep.key_mode, KeyMode::Override) && !ep.api_key.is_empty();
        if use_override {
            req = match protocol {
                Protocol::OpenAI => req.bearer_auth(&ep.api_key),
                Protocol::Claude => req
                    .header("x-api-key", &ep.api_key)
                    .header("anthropic-version", "2023-06-01"),
            };
        } else {
            // 透传客户端鉴权头（authorization / x-api-key）
            // anthropic-version 若客户端未带则补默认值
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

        // 透传客户端业务头（剔除 hop-by-hop 与鉴权头，避免冲突）
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
            resp,
        })
    }
}

pub struct UpstreamResponse {
    pub status: StatusCode,
    pub is_stream: bool,
    pub headers: HeaderMap,
    pub resp: reqwest::Response,
}

impl UpstreamResponse {
    // 构造返回给客户端的 axum Response
    // 非流式：读取完整 body 后返回
    // 流式：把上游字节流接到 axum Body，不缓冲整体内容
    pub async fn into_axum(self) -> Response<Body> {
        let mut builder = Response::builder().status(self.status);

        // 透传响应头（剔除 hop-by-hop）
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
            let stream = self.resp.bytes_stream();
            let body = Body::from_stream(stream);
            builder.body(body).unwrap()
        } else {
            let bytes = self.resp.bytes().await.unwrap_or_else(|_| Bytes::new());
            builder.body(Body::from(bytes)).unwrap()
        }
    }
}
