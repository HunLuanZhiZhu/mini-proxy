// axum 路由与请求处理

use crate::config::{Config, Endpoint};
use crate::protocol::Protocol;
use crate::retry::{dispatch, DispatchOutcome};
use crate::upstream::UpstreamClient;
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, Method, Response, StatusCode};
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
    method: Method,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let protocol = match Protocol::from_path(&path) {
        Some(p) => p,
        // 未识别路径（GET /v1/models、POST /v1/messages/count_tokens 等）：原样透传
        None => {
            return aux_passthrough(&state, method, &path, query.as_deref(), &headers, body).await
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

    // 清洗请求体中 content 为空/空白的 input(messages) 项
    if state.config.server.clean_empty_content {
        clean_empty_messages(&mut parsed, protocol);
    }

    // 思考强度注入：默认按协议强制覆盖到最高档，可配置 passthrough 透传
    let effort = endpoint
        .thinking_effort
        .clone()
        .unwrap_or_else(|| protocol.default_effort().to_string());
    inject_thinking(&mut parsed, protocol, &effort);

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

// 未识别路径的透传：方法/路径/查询串/body 原样转发给第一个配置了 [provider.aux] 的供应商
// 不改 body、不做模型映射、不重试；没配 [provider.aux] 时维持原来的 404
async fn aux_passthrough(
    state: &AppState,
    method: Method,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let mut candidates = state
        .config
        .provider
        .iter()
        .filter_map(|p| p.aux_endpoint().map(|ep| (p.name.clone(), ep)));
    let Some((name, ep)) = candidates.next() else {
        tracing::warn!(path, "无法识别协议路径，且没有供应商配置 [provider.aux]");
        return error_response(StatusCode::NOT_FOUND, "不支持的请求路径");
    };
    if candidates.next().is_some() {
        tracing::warn!(provider = %name, "多个供应商配置了 [provider.aux]，仅使用第一个");
    }

    tracing::info!(
        provider = %name,
        method = %method,
        path,
        channel = %ep.base_url,
        "辅助路径透传"
    );

    match state
        .client
        .send_aux(&ep, method, path, query, &body, headers)
        .await
    {
        Ok(resp) => resp.into_axum().await,
        Err(e) => {
            tracing::warn!(error = %e, "辅助路径透传失败");
            error_response(StatusCode::BAD_GATEWAY, &format!("透传失败: {}", e))
        }
    }
}

// 按协议 + 模型在 providers 中查找第一个匹配的 endpoint
// 返回合并后的 Endpoint（provider 级 + endpoint 级）
// 模型未在任何供应商的 models 里注册时，兜底走第一个配置了该协议端点的供应商
// （把常用网关放 [[provider]] 第一位即可承接所有未注册模型）
fn pick_endpoint(cfg: &Config, protocol: Protocol, model: &str) -> Option<Endpoint> {
    for p in &cfg.provider {
        let ep = match protocol {
            Protocol::OpenAI => p.openai_endpoint(),
            Protocol::Anthropic => p.anthropic_endpoint(),
            Protocol::Responses => p.responses_endpoint(),
        };
        if let Some(ep) = ep {
            if ep.models.iter().any(|m| m == model) {
                return Some(ep);
            }
        }
    }
    let fallback = cfg.provider.iter().find_map(|p| match protocol {
        Protocol::OpenAI => p.openai_endpoint(),
        Protocol::Anthropic => p.anthropic_endpoint(),
        Protocol::Responses => p.responses_endpoint(),
    });
    if fallback.is_some() {
        tracing::info!(model = %model, ?protocol, "模型未注册，回退到第一个供应商");
    }
    fallback
}

// 清洗请求体：
// 1. Responses 协议：补全 input 项缺失的 type: "message" 字段。
// 2. OpenAI Chat：assistant 的 tool_calls/function_call 是语义载荷；即使 content 为空也必须保留。
//    对这类消息把空/空白字符串 content 规范化为 null，避免生成悬空 tool result。
// 3. tool + tool_call_id 同样属于语义消息，不能仅因 content 为空被删除。
// 4. 只有没有其它语义载荷的纯空/空白 content 消息才允许删除。
fn clean_empty_messages(parsed: &mut Value, protocol: Protocol) {
    let field = match protocol {
        Protocol::Responses => "input",
        _ => "messages",
    };

    let Some(arr) = parsed.get_mut(field).and_then(|v| v.as_array_mut()) else {
        return;
    };

    if matches!(protocol, Protocol::Responses) {
        let mut patched = 0;
        for item in arr.iter_mut() {
            if let Some(obj) = item.as_object_mut() {
                if !obj.contains_key("type") {
                    obj.insert("type".into(), Value::String("message".into()));
                    patched += 1;
                }
            }
        }
        if patched > 0 {
            tracing::info!(patched, "已补全 input 项缺失的 type: \"message\"");
        }
    }

    // OpenAI Chat 的 tool-call assistant 使用 null content 更稳定；null 本身保持不变。
    if matches!(protocol, Protocol::OpenAI) {
        for item in arr.iter_mut() {
            let Some(obj) = item.as_object_mut() else {
                continue;
            };
            let is_assistant = obj.get("role").and_then(Value::as_str) == Some("assistant");
            let has_tool_calls = obj
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| !calls.is_empty());
            let has_function_call = obj
                .get("function_call")
                .is_some_and(|call| !call.is_null());

            if is_assistant && (has_tool_calls || has_function_call) {
                if obj
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.trim().is_empty())
                {
                    obj.insert("content".into(), Value::Null);
                }
            }
        }
    }

    let before = arr.len();
    arr.retain(|item| {
        let role = item.get("role").and_then(Value::as_str);

        // assistant 的 tool_calls/function_call 是消息主体，绝不能因文本为空而删除。
        if role == Some("assistant") {
            let has_tool_calls = item
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| !calls.is_empty());
            let has_function_call = item
                .get("function_call")
                .is_some_and(|call| !call.is_null());
            if has_tool_calls || has_function_call {
                return true;
            }
        }

        // tool result 与前序 tool call 通过 tool_call_id 配对；空结果也必须保留。
        if role == Some("tool")
            && item
                .get("tool_call_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
        {
            return true;
        }

        match item.get("content") {
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(Value::Array(a)) => a.iter().any(|c| {
                if let Some(t) = c.get("text").and_then(Value::as_str) {
                    !t.trim().is_empty()
                } else if let Some(t) = c.get("content").and_then(Value::as_str) {
                    !t.trim().is_empty()
                } else {
                    true
                }
            }),
            None => true,
            _ => true,
        }
    });

    let removed = before - arr.len();
    if removed > 0 {
        tracing::info!(field, removed, "已清洗无其它语义载荷的空白 content 消息项");
    }
}

// 思考强度注入：读取客户端原值 → 按 effort 强制覆盖（passthrough 时不改）
// 三协议路径：
//   OpenAI    → 顶层 reasoning_effort
//   Responses → reasoning.effort
//   Anthropic → output_config.effort + thinking.type="adaptive"
fn inject_thinking(parsed: &mut Value, protocol: Protocol, effort: &str) {
    let original = read_effort(parsed, protocol);

    if effort == "passthrough" {
        tracing::info!(original_effort = %original_or_unset(&original), "思考强度透传（未修改）");
        return;
    }

    match protocol {
        Protocol::OpenAI => {
            if let Some(obj) = parsed.as_object_mut() {
                obj.insert("reasoning_effort".into(), Value::String(effort.into()));
            }
        }
        Protocol::Responses => {
            if let Some(obj) = parsed.as_object_mut() {
                let reasoning = obj
                    .entry("reasoning")
                    .or_insert_with(|| Value::Object(Default::default()));
                if let Some(r) = reasoning.as_object_mut() {
                    r.insert("effort".into(), Value::String(effort.into()));
                }
            }
        }
        Protocol::Anthropic => {
            if let Some(obj) = parsed.as_object_mut() {
                // output_config.effort
                let cfg = obj
                    .entry("output_config")
                    .or_insert_with(|| Value::Object(Default::default()));
                if let Some(c) = cfg.as_object_mut() {
                    c.insert("effort".into(), Value::String(effort.into()));
                }
                // thinking.type = "adaptive"
                let thinking = obj
                    .entry("thinking")
                    .or_insert_with(|| Value::Object(Default::default()));
                if let Some(t) = thinking.as_object_mut() {
                    t.insert("type".into(), Value::String("adaptive".into()));
                }
                // adaptive 思考需足够 max_tokens，过小则保护性提升
                if let Some(mt) = obj.get("max_tokens").and_then(|v| v.as_u64()) {
                    if mt < 1024 {
                        obj.insert("max_tokens".into(), Value::Number(1024.into()));
                    }
                }
            }
        }
    }

    tracing::info!(
        original_effort = %original_or_unset(&original),
        new_effort = %effort,
        "思考强度已注入"
    );
}

// 读取客户端请求中原有的思考强度档位
fn read_effort(parsed: &Value, protocol: Protocol) -> Option<String> {
    match protocol {
        Protocol::OpenAI => parsed
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        Protocol::Responses => parsed
            .get("reasoning")
            .and_then(|r| r.get("effort"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        Protocol::Anthropic => parsed
            .get("output_config")
            .and_then(|c| c.get("effort"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

// 原值为空时显示 "未设置"，便于日志可读
fn original_or_unset(original: &Option<String>) -> String {
    match original {
        Some(s) if !s.is_empty() => s.clone(),
        _ => "未设置".to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_tool_call_assistant_and_normalizes_blank_content_to_null() {
        let mut body = json!({
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                }]
            }]
        });

        clean_empty_messages(&mut body, Protocol::OpenAI);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0]["content"].is_null());
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
    }

    #[test]
    fn keeps_tool_call_assistant_with_whitespace_content() {
        let mut body = json!({
            "messages": [{
                "role": "assistant",
                "content": "   \n\t ",
                "tool_calls": [{
                    "id": "call_2",
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                }]
            }]
        });

        clean_empty_messages(&mut body, Protocol::OpenAI);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0]["content"].is_null());
    }

    #[test]
    fn keeps_existing_null_tool_call_assistant_unchanged() {
        let mut body = json!({
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_3",
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                }]
            }]
        });

        clean_empty_messages(&mut body, Protocol::OpenAI);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0]["content"].is_null());
    }

    #[test]
    fn keeps_legacy_function_call_assistant_and_normalizes_content() {
        let mut body = json!({
            "messages": [{
                "role": "assistant",
                "content": "",
                "function_call": {"name": "shell", "arguments": "{}"}
            }]
        });

        clean_empty_messages(&mut body, Protocol::OpenAI);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0]["content"].is_null());
        assert_eq!(messages[0]["function_call"]["name"], "shell");
    }

    #[test]
    fn keeps_blank_tool_result_when_tool_call_id_exists() {
        let mut body = json!({
            "messages": [{
                "role": "tool",
                "tool_call_id": "call_1",
                "content": ""
            }]
        });

        clean_empty_messages(&mut body, Protocol::OpenAI);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["tool_call_id"], "call_1");
    }

    #[test]
    fn removes_plain_blank_messages_without_semantic_payload() {
        let mut body = json!({
            "messages": [
                {"role": "assistant", "content": ""},
                {"role": "user", "content": "   "},
                {"role": "user", "content": "keep me"}
            ]
        });

        clean_empty_messages(&mut body, Protocol::OpenAI);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "keep me");
    }
}
