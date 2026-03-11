use axum::{Router, routing::get, routing::post, Extension, response::Json, extract::Request, middleware::Next, response::IntoResponse};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{CorsLayer, Any};
use serde_json::json;

mod proxy;
mod auth;
mod config;
mod metrics;
mod rate_limit;
mod path_matcher;
mod load_balancer;
mod websocket;
mod health_check;
mod nacos;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let settings = config::load_settings()?;

    // 路由规则 + 健康检查
    let (route_rules, health_config) = config::load_route_rules().unwrap_or_else(|e| {
        tracing::warn!("加载路由规则失败: {}，使用默认配置", e);
        (Vec::new(), health_check::HealthCheckConfig::default())
    });
    let shared_rules = config::create_shared_route_rules(route_rules);

    // Nacos 集成（配置开关控制）
    nacos::init_if_enabled(&settings, &shared_rules).await;

    // 后台服务
    let health_status = health_check::create_health_status();
    health_check::start_health_checker(health_config, shared_rules.clone(), health_status.clone());
    config::start_route_watcher(shared_rules.clone());

    // 构建并启动服务
    let app = build_router(&settings, shared_rules, health_status);
    start_server(app, &settings.gateway_bind).await
}

/// 构建 axum Router（路由 + 中间件 + 扩展注入）
fn build_router(
    settings: &config::Settings,
    shared_rules: config::SharedRouteRules,
    health_status: health_check::SharedHealthStatus,
) -> Router {
    let decoding_key = Arc::new(
        jsonwebtoken::DecodingKey::from_secret(settings.jwt_decoding_key.as_bytes())
    );
    let rate_limits = rate_limit::init_rate_limits(settings);

    Router::new()
        .route("/", get(|| async { "Rust Gateway is running 🚀" }))
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
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        )
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

/// 健康检查端点
async fn health_check_endpoint() -> Json<serde_json::Value> {
    Json(json!({ "status": "UP" }))
}

/// 路由热重载端点 POST /_reload
async fn reload_routes(
    Extension(shared): Extension<config::SharedRouteRules>,
) -> impl IntoResponse {
    match config::reload_route_rules(&shared) {
        Ok(count) => (
            axum::http::StatusCode::OK,
            Json(json!({ "status": "ok", "routes_loaded": count })),
        ),
        Err(err) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "message": err })),
        ),
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
