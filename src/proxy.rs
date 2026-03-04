use axum::{
    body::Body,
    extract::Request,
    http::Response,
    routing::any,
    Router, middleware,
};
use reqwest::Client;
use tracing::info;
use crate::config::Settings;
use crate::rate_limit::rate_limit_layer;
use std::sync::Arc;
use std::time::Duration;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use crate::load_balancer::{RoundRobinBalancer, WeightedRandomBalancer, IpHashBalancer, LoadBalancer, WeightedUpstream};
use axum::middleware::Next;
use axum::http::HeaderValue;

// ===== 全局客户端 =====
/// 全局 HTTP 客户端（高并发优化）
pub static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        // 单域名最大空闲连接数，提高并发处理能力
        .pool_max_idle_per_host(1000)
        // 空闲连接在 90 秒后自动回收，防止无限增长
        .pool_idle_timeout(Some(Duration::from_secs(90)))
        // 全局请求超时，避免慢请求阻塞连接池
        .timeout(Duration::from_secs(10))
        // TCP 连接建立超时
        .connect_timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to build HTTP client")
});

// ===== 全局负载均衡器存储 =====
static BALANCERS: Lazy<DashMap<String, Arc<dyn LoadBalancer + Send + Sync>>> = Lazy::new(DashMap::new);

// 标记：当前请求已命中白名单
#[derive(Clone, Copy, Debug)]
pub struct WhitelistBypass;

// ===== 代理服务路由 =====
pub fn router() -> Router {
    use crate::auth::JwtAuth;

    Router::new()
        .route("/*path", any(proxy_handler))
        // 执行顺序（自下而上）：check_whitelist -> JwtAuth -> propagate_auth_headers
        .route_layer(middleware::from_fn(propagate_auth_headers))
        .route_layer(middleware::from_extractor::<JwtAuth>())
        .route_layer(middleware::from_fn(check_whitelist_middleware))
        .layer(axum::middleware::from_fn(rate_limit_layer))
}

// ===== 代理处理器 =====
async fn proxy_handler(req: Request<Body>) -> Response<Body> {
    // 检测 WebSocket 升级请求
    let upgrade_header = req.headers().get(axum::http::header::UPGRADE);
    let is_websocket = upgrade_header
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    tracing::debug!("请求头 Upgrade: {:?}, is_websocket: {}", upgrade_header, is_websocket);

    let settings = req.extensions().get::<Settings>().cloned();
    let route_rules = req.extensions().get::<Vec<crate::config::RouteRule>>().cloned();

    // 去掉 /proxy 前缀
    let full_path = req.uri().path();
    let match_path = full_path.strip_prefix("/proxy").unwrap_or(full_path);
    let query_suffix = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();

    // 选择上游
    let selected: Option<(String, String)> = if let Some(rules) = &route_rules {
        if let Some(best_match) = find_best_match(rules, match_path) {
            let path_variables = best_match.extract_variables(match_path);
            let selected_upstream = get_or_create_balancer(&best_match.upstream, &best_match.strategy)
                .select(None)
                .unwrap_or_else(|| best_match.upstream[0].clone());
            let forward_path = reconstruct_forward_path(match_path, &best_match.prefix, &path_variables);
            Some((selected_upstream, forward_path))
        } else {
            None
        }
    } else {
        None
    };

    let (upstream, forward_path) = match selected {
        Some(v) => v,
        None => {
            return Response::builder()
                .status(502)
                .header(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")
                .body(Body::from(format!("{{\"error\":\"No upstream configured for path: {}\"}}", match_path)))
                .unwrap();
        }
    };

    info!("路径匹配: {} -> {} (转发到: {})", match_path, forward_path, upstream);

    // 如果是 WebSocket 请求，走 WebSocket 代理逻辑
    if is_websocket {
        // WebSocket 需要保留完整路径，如果 forward_path 为空则使用原始路径
        let ws_path = if forward_path.is_empty() {
            match_path.to_string()
        } else {
            forward_path
        };
        let ws_url = format!("{}{}{}",
            upstream.replace("http://", "ws://").replace("https://", "wss://"),
            ws_path,
            query_suffix
        );
        info!("检测到 WebSocket 请求，转发到: {}", ws_url);
        return crate::websocket::handle_websocket(req, ws_url).await;
    }

    // 构建 reqwest 请求
    let mut rb = HTTP_CLIENT
        .request(req.method().clone(), format!("{}{}{}", upstream, forward_path, query_suffix));

    // 设置超时
    if let Some(s) = &settings {
        rb = rb.timeout(s.request_timeout());
    }

    // 复制 headers
    for (name, value) in req.headers().iter() {
        if name == &axum::http::header::HOST { continue; }
        rb = rb.header(name, value);
    }

    // 读取请求体并转换为reqwest::Body
    let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            return Response::builder()
                .status(500)
                .header(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")
                .body(Body::from(format!("{{\"error\":\"Body read error: {}\"}}", err)))
                .unwrap();
        }
    };

    // 流式转发 body
    let resp_result = rb
        .body(body_bytes)
        .send()
        .await;

    match resp_result {
        Ok(resp) => {
            let status = resp.status();
            let headers = resp.headers().clone();

            let mut builder = Response::builder().status(status);

            // 转发响应头
            for (name, value) in headers.iter() {
                builder = builder.header(name, value);
            }

            // 兜底 Content-Type
            if !builder.headers_ref().map(|h| h.contains_key(axum::http::header::CONTENT_TYPE)).unwrap_or(false) {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/octet-stream");
            }

            // 读取响应体
            let bytes = match resp.bytes().await {
                Ok(bytes) => bytes,
                Err(err) => {
                    return Response::builder()
                        .status(500)
                        .header(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")
                        .body(Body::from(format!("{{\"error\":\"Response body error: {}\"}}", err)))
                        .unwrap();
                }
            };

            builder.body(Body::from(bytes)).unwrap()
        }
        Err(err) => Response::builder()
            .status(500)
            .header(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(Body::from(format!("{{\"error\":\"Proxy error: {}\"}}", err)))
            .unwrap(),
    }
}

// ===== 获取或创建负载均衡器 =====
fn get_or_create_balancer(upstreams: &[String], strategy: &str) -> Arc<dyn LoadBalancer + Send + Sync> {
    let key = format!("{}:{}", strategy, upstreams.join(","));
    BALANCERS
        .entry(key.clone())
        .or_insert_with(|| {
            match strategy {
                "random" => Arc::new(WeightedRandomBalancer::new(
                    upstreams.iter().map(|u| WeightedUpstream {
                        url: u.clone(),
                        weight: 1,
                    }).collect()
                )),
                "iphash" => Arc::new(IpHashBalancer::new(upstreams.to_vec())),
                _ => Arc::new(RoundRobinBalancer::new(upstreams.to_vec())), // 默认轮询
            }
        })
        .clone()
}

// ===== 查找最佳匹配规则（预编译正则可选） =====
fn find_best_match<'a>(rules: &'a [crate::config::RouteRule], path: &str) -> Option<&'a crate::config::RouteRule> {
    let mut best_match: Option<&crate::config::RouteRule> = None;
    let mut best_score = 0;

    for rule in rules {
        if rule.matches(path) {
            let score = rule.prefix.iter().map(|p| {
                if p.contains('{') || p.contains('*') || p.contains('?') {
                    1000 + p.len() as i32
                } else { p.len() as i32 }
            }).max().unwrap_or(0);

            if score > best_score {
                best_score = score;
                best_match = Some(rule);
            }
        }
    }

    best_match
}

// ===== 重构转发路径 =====
fn reconstruct_forward_path(
    original_path: &str,
    prefixes: &[String],
    _variables: &std::collections::HashMap<String, String>,
) -> String {
    for prefix in prefixes {
        if original_path.starts_with(prefix) {
            return original_path.strip_prefix(prefix).unwrap_or(original_path).to_string();
        }
    }
    original_path.to_string()
}

// ===== 白名单检查中间件 =====
async fn check_whitelist_middleware(mut req: Request<Body>, next: Next) -> Response<Body> {
    let path = req.uri().path();
    let match_path = path.strip_prefix("/proxy").unwrap_or(path);

    tracing::debug!("白名单检查: 原始路径={}, 匹配路径={}", path, match_path);

    if let Some(rules) = req.extensions().get::<Vec<crate::config::RouteRule>>() {
        // 找到第一个匹配的路由，检查其 whitelist 是否命中
        if let Some(rule) = find_best_match(rules, match_path) {
            tracing::debug!("找到匹配规则: prefix={:?}, whitelist={:?}", rule.prefix, rule.whitelist);
            if let Some(whitelist) = &rule.whitelist {
                // 任意一个白名单模式命中即可
                let hit = whitelist.iter().any(|w| {
                    // 复用 RouteRule 的匹配逻辑
                    // 这里把单个白名单项当作一个前缀来匹配
                    let matched = if w.contains('{') || w.contains('*') || w.contains('?') {
                        crate::path_matcher::RoutePattern::from_pattern(w)
                            .map(|rp| rp.matches(match_path))
                            .unwrap_or(false)
                    } else {
                        match_path == w || match_path.starts_with(&format!("{}/", w))
                    };
                    tracing::debug!("白名单项 '{}' 匹配 '{}': {}", w, match_path, matched);
                    matched
                });
                if hit {
                    tracing::info!("✓ 白名单命中，跳过鉴权: {}", match_path);
                    // 标记跳过鉴权
                    req.extensions_mut().insert(WhitelistBypass);
                } else {
                    tracing::warn!("✗ 白名单未命中: {}", match_path);
                }
            }
        } else {
            tracing::warn!("未找到匹配的路由规则: {}", match_path);
        }
    }

    next.run(req).await
}

// ===== 透传租户和用户id信息中间件 =====
async fn propagate_auth_headers(mut req: Request<Body>, next: Next) -> Response<Body> {
    // 先提取 JWT 信息，避免借用冲突
    let (uid, tenant_id) = if let Some(jwt) = req.extensions().get::<crate::auth::JwtAuth>() {
        (jwt.0.sub.clone(), jwt.0.tenant_id.clone())
    } else {
        (String::new(), String::new())
    };
    
    // 然后修改 headers
    if !uid.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&uid) {
            req.headers_mut().insert("uid", v);
        }
    }
    if !tenant_id.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&tenant_id) {
            req.headers_mut().insert("tenant_id", v);
        }
    }
    
    next.run(req).await
}
