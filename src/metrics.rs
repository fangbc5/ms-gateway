use std::time::Instant;

use prometheus::{Encoder, TextEncoder, IntCounterVec, register_int_counter_vec, register_histogram_vec, HistogramVec};
use once_cell::sync::Lazy;
use axum::{extract::Request, http::StatusCode, middleware::Next, response::IntoResponse};

pub static HTTP_COUNTER: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "gateway_http_requests_total",
        "Total HTTP requests handled",
        &["method", "path", "status"]
    )
    .unwrap()
});

pub static HTTP_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "gateway_request_duration_seconds",
        "Request duration histogram",
        &["method", "path"]
    )
    .unwrap()
});

pub async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, encoder.format_type().to_string())], buffer)
}

// ===== Prometheus 中间件 =====
pub async fn prometheus_middleware(req: Request, next: Next) -> impl IntoResponse {
    let method = req.method().to_string();
    let raw_path = req.uri().path().to_string();

    // 在 handler 执行前，预匹配路由模板作为 metrics label
    let match_path = raw_path.strip_prefix("/proxy").unwrap_or(&raw_path);
    let route_template = resolve_route_template(&req, match_path);

    let start = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16().to_string();

    if raw_path != "/metrics" {
        HTTP_COUNTER.with_label_values(&[&method, &route_template, &status]).inc();
        HTTP_DURATION.with_label_values(&[&method, &route_template]).observe(start.elapsed().as_secs_f64());
    }

    response
}

/// 从请求的路由规则中匹配路由模板路径，避免使用原始路径导致 Prometheus 标签爆炸。
/// 如 `/api/user/123` -> `/api/user/{id}`
fn resolve_route_template(req: &Request, match_path: &str) -> String {
    if let Some(rules) = req.extensions().get::<Vec<crate::config::RouteRule>>() {
        for rule in rules {
            if rule.matches(match_path) {
                // 返回规则的第一个匹配前缀作为模板标签
                return rule.prefix.first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
            }
        }
    }
    "unmatched".to_string()
}