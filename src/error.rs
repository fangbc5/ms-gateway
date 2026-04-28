use axum::{
    http::StatusCode,
    response::IntoResponse,
};
use serde::Serialize;

/// 限流类型
#[derive(Debug)]
pub enum RateLimitKind {
    /// 全局限流
    Global,
    /// 单客户端限流
    Client,
}

/// 网关统一错误响应体
#[derive(Debug, Serialize)]
pub struct GatewayErrorBody {
    pub success: bool,
    pub code: u16,
    pub msg: String,
    #[serde(serialize_with = "serialize_null")]
    pub data: (),
    pub timestamp: i64,
}

fn serialize_null<S>(_: &(), s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    s.serialize_none()
}

impl GatewayErrorBody {
    pub fn new(code: u16, msg: impl Into<String>) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        Self {
            success: false,
            code,
            msg: msg.into(),
            data: (),
            timestamp,
        }
    }
}

/// 网关统一错误枚举
#[derive(Debug)]
pub enum GatewayError {
    /// 502 - 无可用上游
    NoUpstream(String),
    /// 502 - 代理转发失败
    ProxyError(String),
    /// 429 - 限流
    RateLimited(RateLimitKind),
    /// 403 - 管理接口鉴权失败
    Forbidden(String),
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> axum::response::Response {
        let (status, body) = match self {
            GatewayError::NoUpstream(path) => (
                StatusCode::BAD_GATEWAY,
                GatewayErrorBody::new(502, format!("无可用上游: {}", path)),
            ),
            GatewayError::ProxyError(err) => (
                StatusCode::BAD_GATEWAY,
                GatewayErrorBody::new(502, format!("代理转发失败: {}", err)),
            ),
            GatewayError::RateLimited(kind) => {
                let msg = match kind {
                    RateLimitKind::Global => "请求过于频繁（全局限流）",
                    RateLimitKind::Client => "请求过于频繁（客户端限流）",
                };
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    GatewayErrorBody::new(429, msg),
                )
            }
            GatewayError::Forbidden(msg) => (
                StatusCode::FORBIDDEN,
                GatewayErrorBody::new(403, msg),
            ),
        };

        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_body_serialization() {
        let body = GatewayErrorBody::new(502, "test error");
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"code\":502"));
        assert!(json.contains("\"msg\":\"test error\""));
        assert!(json.contains("\"data\":null"));
        assert!(json.contains("\"timestamp\":"));
    }
}
