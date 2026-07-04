// 重试循环：仅同渠道同模型重试，不跨渠道，无 sleep 间隔

use crate::config::Endpoint;
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

// 运行重试循环，返回最终响应或失败构造的错误响应
pub async fn dispatch(
    client: &UpstreamClient,
    ep: &Endpoint,
    protocol: Protocol,
    body: &Bytes,
    headers: &axum::http::HeaderMap,
) -> DispatchOutcome {
    let matcher = ep.status_matcher();
    let total = ep.max_retries + 1;
    let mut last_status: Option<StatusCode> = None;
    let mut last_err: Option<String> = None;

    for attempt in 0..total {
        tracing::info!(
            attempt = attempt + 1,
            total,
            model = tracing::field::Empty,
            channel = tracing::field::Empty,
            "开始第 {} 次尝试，共 {} 次",
            attempt + 1,
            total
        );

        match client.send(ep, protocol, body, headers).await {
            Ok(resp) => {
                let code = resp.status.as_u16();
                tracing::info!(code, upstream_status = code, "上游返回状态码");

                if matcher.matches(code) {
                    tracing::warn!(code, "命中可重试状态码，将重试");
                    last_status = Some(resp.status);
                    // 若已开始流式（200 + SSE），不能再重试，直接返回
                    if resp.is_stream {
                        tracing::warn!("上游已开始流式响应，无法重试，直接透传");
                        let r = resp.into_axum().await;
                        return DispatchOutcome::Ok(r);
                    }
                    // 释放响应 body，进入下次重试
                    drop(resp);
                    continue;
                }

                // 非可重试状态：成功或不可重试错误，直接返回
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

    // 构造错误响应返回给客户端，消息中文
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
