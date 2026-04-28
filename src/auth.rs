use axum::{
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Claims 结构（与 sa-token JwtClaims 兼容）
///
/// sa-token 的 JwtClaims 使用 `sub` 存储 login_id，
/// 业务字段（tenant_id, username, token_type）存储在 `extra` HashMap 中。
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    /// 用户 ID（sa-token 的 login_id）
    pub sub: String,
    /// 过期时间（Unix 时间戳，秒）
    #[serde(default)]
    pub exp: Option<i64>,
    /// 生效时间（Unix 时间戳，秒）
    #[serde(default)]
    pub nbf: Option<i64>,
    /// 签发时间（Unix 时间戳，秒）
    #[serde(default)]
    pub iat: Option<i64>,
    /// JWT ID
    #[serde(default)]
    pub jti: Option<String>,
    /// 签发者
    #[serde(default)]
    pub iss: Option<String>,
    /// 受众
    #[serde(default)]
    pub aud: Option<String>,
    /// 登录类型
    #[serde(default)]
    pub login_type: Option<String>,
    /// 设备标识
    #[serde(default)]
    pub device: Option<String>,
    /// 扩展字段（tenant_id, username, token_type 等）
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl Claims {
    /// 从 extra 获取 tenant_id
    pub fn tenant_id(&self) -> String {
        self.extra
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    /// 从 extra 获取 username
    pub fn username(&self) -> String {
        self.extra
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("missing authorization header")]
    MissingHeader,
    #[error("invalid token")]
    InvalidToken,
    #[error("jwt decode error")]
    DecodeError(#[from] jsonwebtoken::errors::Error),
    #[error("config missing")]
    ConfigMissing,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> axum::response::Response {
        let (status, msg) = match &self {
            AuthError::MissingHeader => (StatusCode::UNAUTHORIZED, "缺少 Authorization 请求头"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "无效的 Token"),
            AuthError::DecodeError(e) => {
                tracing::warn!("🔐 JWT 解码失败: {}", e);
                (StatusCode::UNAUTHORIZED, "Token 已过期或无效")
            }
            AuthError::ConfigMissing => (StatusCode::INTERNAL_SERVER_ERROR, "网关配置缺失"),
        };
        let body = crate::error::GatewayErrorBody::new(status.as_u16(), msg);
        (status, axum::Json(body)).into_response()
    }
}

