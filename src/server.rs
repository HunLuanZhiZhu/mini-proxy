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
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub client: Arc<UpstreamClient>,
}

#[derive(Deserialize)]
struct ModelField {
    model: Option<String>,
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

    // 解析 model 字段用于选渠道
    let model = serde_json::from_slice::<ModelField>(&body)
        .ok()
        .and_then(|m| m.model)
        .unwrap_or_default();

    tracing::info!(
        path = %path,
        protocol = ?protocol,
        model = %model,
        "收到请求"
    );

    let endpoint = match pick_endpoint(&state.config, protocol, &model) {
        Some(ep) => ep,
        None => {
            tracing::warn!(?protocol, model = %model, "无可用渠道");
            return error_response(
                StatusCode::NOT_FOUND,
                &format!("未找到匹配的渠道：协议 {:?}，模型 {}", protocol, model),
            );
        }
    };

    let req_id = uuid::Uuid::now_v7();
    let span = tracing::info_span!(
        "request",
        request_id = %req_id,
        protocol = ?protocol,
        model = %model,
        channel = %endpoint.base_url,
    );
    let _enter = span.enter();

    match dispatch(&state.client, endpoint, protocol, &body, &headers).await {
        DispatchOutcome::Ok(r) => r,
        DispatchOutcome::Failed { status, body } => {
            let mut resp = Response::new(Body::from(body));
            *resp.status_mut() = status;
            resp
        }
    }
}

// 按协议 + 模型在 providers 中查找第一个匹配的 endpoint
fn pick_endpoint<'a>(cfg: &'a Config, protocol: Protocol, model: &str) -> Option<&'a Endpoint> {
    for p in &cfg.provider {
        let ep = match protocol {
            Protocol::OpenAI => p.openai.as_ref(),
            Protocol::Claude => p.claude.as_ref(),
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
