// axum 路由与请求处理

use crate::config::{Config, Endpoint};
use crate::protocol::Protocol;
use crate::retry::{dispatch, DispatchOutcome};
use crate::upstream::UpstreamClient;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::{any, post};
use axum::Router;
use bytes::Bytes;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub client: Arc<UpstreamClient>,
}

pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/", post(handle))
        .route("/*path", any(handle))
        .with_state(state)
}

async fn handle(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let protocol = match Protocol::from_path(&path) {
        Some(p) => p,
        None => {
            tracing::warn!(path = %path, "无法识别协议路径");
            return error_response(StatusCode::NOT_FOUND, "不支持的请求路径");
        }
    };

    // 解析 body 取 model 字段用于选渠道，并做模型名映射
    let mut parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "请求 body 不是合法 JSON");
            return error_response(StatusCode::BAD_REQUEST, "请求 body 不是合法 JSON");
        }
    };

    let client_model = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    tracing::info!(
        path = %path,
        protocol = ?protocol,
        model = %client_model,
        "收到请求"
    );

    let endpoint = match pick_endpoint(&state.config, protocol, &client_model) {
        Some(ep) => ep,
        None => {
            tracing::warn!(?protocol, model = %client_model, "无可用渠道");
            return error_response(
                StatusCode::NOT_FOUND,
                &format!("未找到匹配的渠道：协议 {:?}，模型 {}", protocol, client_model),
            );
        }
    };

    // 模型名映射：客户端模型名 → 上游模型名
    let upstream_model = endpoint.map_model(&client_model);
    if upstream_model != client_model {
        if let Some(obj) = parsed.as_object_mut() {
            obj.insert("model".into(), Value::String(upstream_model.clone()));
        }
        tracing::info!(client_model = %client_model, upstream_model = %upstream_model, "模型名已映射");
    }
    let body_bytes = serde_json::to_vec(&parsed).unwrap_or_else(|_| body.to_vec());
    let body_bytes = Bytes::from(body_bytes);

    let req_id = uuid::Uuid::now_v7();
    let span = tracing::info_span!(
        "request",
        request_id = %req_id,
        protocol = ?protocol,
        model = %client_model,
        channel = %endpoint.base_url,
    );
    let _enter = span.enter();

    match dispatch(&state.client, &endpoint, protocol, &body_bytes, &headers).await {
        DispatchOutcome::Ok(r) => r,
        DispatchOutcome::Failed { status, body } => {
            let mut resp = Response::new(Body::from(body));
            *resp.status_mut() = status;
            resp
        }
    }
}

// 按协议 + 模型在 providers 中查找第一个匹配的 endpoint
// 返回合并后的 Endpoint（provider 级 + endpoint 级）
fn pick_endpoint(cfg: &Config, protocol: Protocol, model: &str) -> Option<Endpoint> {
    for p in &cfg.provider {
        let ep = match protocol {
            Protocol::OpenAI => p.openai_endpoint(),
            Protocol::Claude => p.claude_endpoint(),
        };
        if let Some(ep) = ep {
            if ep.models.iter().any(|m| m == model) {
                return Some(ep);
            }
        }
    }
    None
}

fn error_response(status: StatusCode, msg: &str) -> Response<Body> {
    let body = format!(
        r#"{{"error":{{"message":"{}","type":"proxy_error"}}}}"#,
        msg
    );
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}
