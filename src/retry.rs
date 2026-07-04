// 重试循环：仅同渠道同模型重试，不跨渠道，无 sleep 间隔
// 判断重试依据：HTTP 状态码范围 + 响应 body 中的业务错误码

use crate::config::{is_always_skip, Endpoint};
use crate::protocol::Protocol;
use crate::upstream::UpstreamClient;
use axum::body::Body;
use axum::http::StatusCode;
use axum::http::Response;
use bytes::Bytes;

pub enum DispatchOutcome {
    Ok(Response<Body>),
    Failed { status: StatusCode, body: Bytes },
}

pub async fn dispatch(
    client: &UpstreamClient,
    ep: &Endpoint,
    protocol: Protocol,
    body: &Bytes,
    headers: &axum::http::HeaderMap,
) -> DispatchOutcome {
    let matcher = ep.status_matcher();
    let retry_codes = ep.retry_codes();
    let total = ep.max_retries + 1;
    let mut last_status: Option<StatusCode> = None;
    let mut last_err: Option<String> = None;

    for attempt in 0..total {
        tracing::info!(
            attempt = attempt + 1,
            total,
            "开始第 {} 次尝试，共 {} 次",
            attempt + 1,
            total
        );

        match client.send(ep, protocol, body, headers).await {
            Ok(mut resp) => {
                let code = resp.status.as_u16();
                tracing::info!(code, upstream_status = code, "上游返回状态码");

                // 永远跳过（504/524）：直接返回，不重试
                if is_always_skip(code) {
                    tracing::warn!(code, "命中永远不重试状态码，直接返回");
                    let r = resp.into_axum().await;
                    return DispatchOutcome::Ok(r);
                }

                // 流式响应已开始：不能再重试，直接返回
                if resp.is_stream {
                    tracing::info!("上游已开始流式响应，直接透传");
                    let r = resp.into_axum().await;
                    return DispatchOutcome::Ok(r);
                }

                // 非流式：预读 body 以检查业务错误码
                resp.preload_body().await;

                // 检查业务错误码（error.code 字段）
                let biz_code = resp.extract_error_code();
                let biz_retry = biz_code
                    .map(|c| retry_codes.contains(&c))
                    .unwrap_or(false);

                if biz_retry {
                    tracing::warn!(biz_code = biz_code, "命中可重试业务错误码，将重试");
                    last_status = Some(resp.status);
                    drop(resp);
                    continue;
                }

                // HTTP 状态码命中可重试范围
                if matcher.matches(code) {
                    tracing::warn!(code, "命中可重试状态码，将重试");
                    last_status = Some(resp.status);
                    drop(resp);
                    continue;
                }

                // 非可重试：成功或不可重试错误，直接返回
                let r = resp.into_axum().await;
                return DispatchOutcome::Ok(r);
            }
            Err(e) => {
                tracing::warn!(error = %e, "网络错误，将重试");
                last_err = Some(e.to_string());
                continue;
            }
        }
    }

    tracing::warn!(?last_status, ?last_err, "同渠道重试已耗尽");

    let msg = match (last_status, last_err) {
        (Some(s), _) => format!("上游返回 {}，重试 {} 次后仍失败", s, ep.max_retries),
        (None, Some(e)) => format!("网络错误，重试 {} 次后仍失败：{}", ep.max_retries, e),
        (None, None) => "未知错误".to_string(),
    };
    let body = Bytes::from(format!(
        r#"{{"error":{{"message":"{}","type":"upstream_error"}}}}"#,
        msg
    ));
    DispatchOutcome::Failed {
        status: StatusCode::BAD_GATEWAY,
        body,
    }
}
