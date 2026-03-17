use axum::{
    async_trait,
    extract::{FromRequestParts},
    http::{request::Parts, StatusCode},
    response::{IntoResponse},
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation, TokenData};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,           // 用户 ID
    pub exp: usize,            // 过期时间（秒）
    #[serde(default)]
    pub tenant_id: String,     // 多租户 ID
    #[serde(default)]
    pub username: String,      // 用户名
    #[serde(default)]
    pub token_type: String,    // token 类型
    #[serde(default)]
    pub iat: i64,              // 签发时间
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
        let (status, msg) = match self {
            AuthError::MissingHeader => (StatusCode::UNAUTHORIZED, "Missing authorization header"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid token"),
            AuthError::DecodeError(_) => (StatusCode::UNAUTHORIZED, "Token decode error"),
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
            return Ok(JwtAuth(Claims { sub: String::new(), exp: 0, tenant_id: String::new(), username: String::new(), token_type: String::new(), iat: 0 }));
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
