use axum::{
    async_trait,
    extract::{FromRequestParts},
    http::{request::Parts, StatusCode},
    response::{IntoResponse},
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation, TokenData};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
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
            AuthError::MissingHeader => (StatusCode::UNAUTHORIZED, "Missing authorization header"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid token"),
            AuthError::DecodeError(e) => {
                tracing::warn!("🔐 JWT 解码失败: {}", e);
                (StatusCode::UNAUTHORIZED, "Token decode error")
            }
            AuthError::ConfigMissing => (StatusCode::INTERNAL_SERVER_ERROR, "Config missing"),
        };
        (status, msg).into_response()
    }
}

/// Extractor: 从请求 header 中验证 JWT 并把 Claims 放进请求扩展里
#[derive(Debug, Clone)]
pub struct JwtAuth(pub Claims);

#[async_trait]
impl<S> FromRequestParts<S> for JwtAuth
where
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // 白名单标记则跳过鉴权，返回空 Claims
        if parts.extensions.get::<crate::proxy::WhitelistBypass>().is_some() {
            return Ok(JwtAuth(Claims {
                sub: String::new(),
                exp: None,
                nbf: None,
                iat: None,
                jti: None,
                iss: None,
                aud: None,
                login_type: None,
                device: None,
                extra: HashMap::new(),
            }));
        }

        // 使用预构造的 DecodingKey（启动时注入，避免每次请求重复构建）
        let decoding_key = parts
            .extensions
            .get::<Arc<DecodingKey>>()
            .ok_or(AuthError::ConfigMissing)?
            .clone();

        let auth_header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(AuthError::MissingHeader)?;

        if !auth_header.starts_with("Bearer ") {
            return Err(AuthError::InvalidToken);
        }
        let token = auth_header.trim_start_matches("Bearer ").trim();

        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;

        let token_data: TokenData<Claims> = decode(
            token,
            &decoding_key,
            &validation,
        )?;

        let claims = token_data.claims;
        
        // 将解析后的 Claims 存储到 extensions 中，供后续中间件使用
        parts.extensions.insert(JwtAuth(claims.clone()));

        Ok(JwtAuth(claims))
    }
}
