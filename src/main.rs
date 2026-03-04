use axum::{Router, routing::get, Extension};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use std::net::SocketAddr;

mod proxy;
mod auth;
mod config;
mod metrics;
mod rate_limit;
mod path_matcher;
mod load_balancer;
mod websocket;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志：若无 RUST_LOG 则默认 info
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        )
        .init();
    // 加载环境配置
    let settings = config::load_settings()?;
    // 构建速率限制器（全局与每客户端），注入到扩展
    let rate_limits = rate_limit::init_rate_limits(&settings);

    // 加载路由前缀规则，并注入扩展
    let route_rules = config::load_route_rules().unwrap_or_default();

    // 路由
    let app = Router::new()
        .route("/", get(|| async { "Rust Gateway is running 🚀" }))
        .route("/metrics", get(metrics::metrics_handler))
        .merge(proxy::router())
        .layer(axum::middleware::from_fn(metrics::prometheus_middleware))
        .layer(Extension(settings.clone()))
        .layer(Extension(rate_limits.clone()))
        .layer(Extension(route_rules));

    // 启动服务（带客户端地址信息）
    let listener = TcpListener::bind(&settings.gateway_bind).await?;
    tracing::info!("🚀 Gateway listening on http://{}", listener.local_addr()?);

    let make_svc = app.into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, make_svc).await?;
    Ok(())
}
