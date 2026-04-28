use axum::{
    extract::Request, middleware::Next, response::IntoResponse, response::Json, routing::get,
    routing::post, Extension, Router,
};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::EnvFilter;

mod auth;
mod config;
mod error;
mod health_check;
mod load_balancer;
mod metrics;
mod nacos;
mod path_matcher;
mod proxy;
mod rate_limit;
mod websocket;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let settings = config::load_settings()?;

    // 先创建空的共享路由规则容器
    let shared_rules = config::create_shared_route_rules(Vec::new());

    // Nacos 优先：如果 Nacos 成功加载了路由配置，跳过文件配置
    let nacos_active = nacos::init_if_enabled(&settings, &shared_rules).await;

    // 仅当 Nacos 未激活时，才加载本地 routes.toml 和启动文件监听
    let health_config = if nacos_active {
        tracing::info!("📡 路由由 Nacos 配置中心管理，跳过本地 routes.toml");
        health_check::HealthCheckConfig::default()
    } else {
        let (route_rules, hc) = config::load_route_rules().unwrap_or_else(|e| {
            tracing::warn!("加载路由规则失败: {}，使用默认配置", e);
            (Vec::new(), health_check::HealthCheckConfig::default())
        });
        shared_rules.store(std::sync::Arc::new(route_rules));
        config::start_route_watcher(shared_rules.clone());
        hc
    };

    // 后台服务
    let health_status = health_check::create_health_status();
    health_check::start_health_checker(health_config, shared_rules.clone(), health_status.clone());

    // 构建并启动服务
    let app = build_router(&settings, shared_rules, health_status);
    start_server(app, &settings.server.bind_addr()).await
}

/// 构建 axum Router（路由 + 中间件 + 扩展注入）
fn build_router(
    settings: &config::Settings,
    shared_rules: config::SharedRouteRules,
    health_status: health_check::SharedHealthStatus,
) -> Router {
    let decoding_key = Arc::new(jsonwebtoken::DecodingKey::from_secret(
        settings.jwt_secret.as_bytes(),
    ));
    let rate_limits = rate_limit::init_rate_limits(settings);

    Router::new()
        .route("/", get(landing_page))
        .route("/health", get(health_check_endpoint))
        .route("/metrics", get(metrics::metrics_handler))
        .route("/_reload", post(reload_routes))
        .merge(proxy::router())
        .layer(build_cors(settings))
        .layer(axum::middleware::from_fn(request_id_middleware))
        .layer(axum::middleware::from_fn(metrics::prometheus_middleware))
        .layer(Extension(settings.clone()))
        .layer(Extension(rate_limits))
        .layer(Extension(shared_rules))
        .layer(Extension(health_status))
        .layer(Extension(decoding_key))
}

/// 启动 HTTP 服务（带优雅关闭）
async fn start_server(app: Router, bind_addr: &str) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!("🚀 Gateway listening on http://{}", listener.local_addr()?);

    let make_svc = app.into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, make_svc)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// 初始化日志
fn init_tracing() {
    // 优先读 APP__LOG_LEVEL（与微服务统一），兼容 RUST_LOG
    let filter = std::env::var("APP__LOG_LEVEL")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&filter))
        .init();
}

/// 构建 CORS 层
fn build_cors(settings: &config::Settings) -> CorsLayer {
    match &settings.cors_allowed_origins {
        Some(origins) if !origins.is_empty() && origins != "*" => {
            let parsed: Vec<axum::http::HeaderValue> = origins
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            tracing::info!("CORS 限制为指定源: {}", origins);
            CorsLayer::new()
                .allow_origin(parsed)
                .allow_methods(Any)
                .allow_headers(Any)
        }
        _ => {
            tracing::info!("CORS 允许所有源");
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
        }
    }
}

/// 首页落地页
async fn landing_page() -> impl IntoResponse {
    axum::response::Html(include_str!("../static/index.html"))
}

/// 健康检查端点
async fn health_check_endpoint() -> Json<serde_json::Value> {
    Json(json!({ "status": "UP" }))
}

/// 路由热重载端点 POST /_reload
async fn reload_routes(
    Extension(shared): Extension<config::SharedRouteRules>,
    Extension(settings): Extension<config::Settings>,
    req: axum::extract::Request,
) -> axum::response::Response {
    // 管理接口鉴权：如果配置了 admin_token，则要求携带 X-Admin-Token
    if let Some(ref expected_token) = settings.admin_token {
        let provided = req
            .headers()
            .get("X-Admin-Token")
            .and_then(|v| v.to_str().ok());
        match provided {
            Some(token) if token == expected_token => {}
            _ => {
                return error::GatewayError::Forbidden(
                    "管理接口鉴权失败，请提供正确的 X-Admin-Token".to_string(),
                )
                .into_response();
            }
        }
    }

    match config::reload_route_rules(&shared) {
        Ok(count) => (
            axum::http::StatusCode::OK,
            Json(json!({ "status": "ok", "routes_loaded": count })),
        )
            .into_response(),
        Err(err) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "message": err })),
        )
            .into_response(),
    }
}

/// x-request-id 中间件
async fn request_id_middleware(mut req: Request, next: Next) -> impl IntoResponse {
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if let Ok(val) = axum::http::HeaderValue::from_str(&request_id) {
        req.headers_mut().insert("x-request-id", val);
    }

    let mut response = next.run(req).await;

    if let Ok(val) = axum::http::HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", val);
    }

    response
}

/// 优雅关闭信号处理
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("无法安装 Ctrl+C 信号处理器");
        tracing::info!("收到 Ctrl+C 信号，开始优雅关闭...");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("无法安装 SIGTERM 信号处理器")
            .recv()
            .await;
        tracing::warn!("收到 SIGTERM 信号，开始优雅关闭...");
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("服务器正在关闭...");
}
