use axum::{
    body::Body,
    extract::Request,
    http::Response,
    response::IntoResponse,
    routing::any,
    Router, middleware,
};
use reqwest::Client;
use tracing::info;
use crate::config::Settings;
use crate::rate_limit::rate_limit_layer;
use std::sync::Arc;
use std::net::SocketAddr;
use std::time::Duration;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use crate::load_balancer::{RoundRobinBalancer, WeightedRandomBalancer, IpHashBalancer, LoadBalancer, WeightedUpstream};
use axum::middleware::Next;
use axum::http::HeaderValue;
use std::time::Instant;

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
    Router::new()
        .route("/*path", any(proxy_handler))
        // 执行顺序（自下而上）：check_whitelist -> auth_and_propagate（合并鉴权+透传）
        .route_layer(middleware::from_fn(auth_and_propagate))
        .route_layer(middleware::from_fn(check_whitelist_middleware))
        .layer(axum::middleware::from_fn(rate_limit_layer))
}

// ===== W3C Trace Context 工具函数 =====

/// 从 traceparent header 解析 trace_id
fn parse_trace_id(traceparent: &str) -> Option<String> {
    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() >= 3 && parts[1].len() == 32 {
        Some(parts[1].to_string())
    } else {
        None
    }
}

/// 生成 trace_id（32位 hex）
fn generate_trace_id() -> String {
    uuid::Uuid::new_v4().as_simple().to_string()
}

/// 生成 span_id（16位 hex）
fn generate_span_id() -> String {
    uuid::Uuid::new_v4().as_simple().to_string()[..16].to_string()
}

/// 多级慢请求告警标签
fn slow_request_level(duration_ms: u128) -> Option<&'static str> {
    match duration_ms {
        10_000.. => Some("\u{1f534} CRITICAL >10s"),
        5_000..=9_999 => Some("\u{1f534} VERY_SLOW >5s"),
        3_000..=4_999 => Some("\u{1f7e0} SLOW >3s"),
        2_000..=2_999 => Some("\u{1f7e0} SLOW >2s"),
        1_000..=1_999 => Some("\u{1f7e1} SLOW >1s"),
        500..=999 => Some("\u{1f7e1} SLOW >500ms"),
        200..=499 => Some("\u{1f7e2} SLOW >200ms"),
        _ => None,
    }
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
    // 从 SharedRouteRules（ArcSwap）读取最新路由规则（支持热重载）
    let route_rules = req.extensions()
        .get::<crate::config::SharedRouteRules>()
        .map(|shared| shared.load_full());
    // 提取客户端地址（用于 IP Hash 负载均衡）
    let client_addr = req.extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);

    // 去掉 /proxy 前缀
    let full_path = req.uri().path();
    let match_path = full_path.strip_prefix("/proxy").unwrap_or(full_path).to_string();
    let query_suffix = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    let req_method = req.method().clone();

    // 提取或生成 trace context（从入站 traceparent header）
    let trace_id = req.headers().get("traceparent")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_trace_id)
        .unwrap_or_else(generate_trace_id);
    let span_id = generate_span_id();
    let traceparent = format!("00-{}-{}-01", trace_id, span_id);
    let trace_short = trace_id[..8.min(trace_id.len())].to_string();

    // 读取健康状态
    let health_status = req.extensions()
        .get::<crate::health_check::SharedHealthStatus>()
        .cloned();

    // 选择上游（支持 Nacos 服务发现 + 健康过滤 + 负载均衡）
    let selected: Option<(String, String)> = if let Some(rules) = &route_rules {
        if let Some(best_match) = find_best_match(rules, &match_path) {
            let path_variables = best_match.extract_variables(&match_path);

            // 从 Nacos 服务发现获取上游（带权重），或使用路由规则中的 upstream（默认权重1）
            let weighted_upstreams: Vec<WeightedUpstream> = if let Some(ref svc_name) = best_match.service_name {
                let nacos_ups = crate::nacos::instances_to_weighted_upstreams(svc_name);
                if nacos_ups.is_empty() {
                    tracing::warn!("⚠️ 服务 {} 无可用实例，请检查 Nacos 注册状态", svc_name);
                }
                nacos_ups
            } else {
                best_match.upstream.iter().map(|u| WeightedUpstream {
                    url: u.clone(),
                    weight: 1,
                }).collect()
            };

            if weighted_upstreams.is_empty() {
                None
            } else {
                // 过滤不健康的上游
                let healthy_upstreams = if let Some(ref hs) = health_status {
                    let healthy_urls: Vec<String> = crate::health_check::filter_healthy_upstreams(
                        &weighted_upstreams.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
                        hs,
                    );
                    // 保留在健康列表中的 WeightedUpstream
                    weighted_upstreams.into_iter()
                        .filter(|u| healthy_urls.contains(&u.url))
                        .collect::<Vec<_>>()
                } else {
                    weighted_upstreams
                };

                let selected_upstream = get_or_create_balancer(&best_match.prefix, &healthy_upstreams, &best_match.strategy)
                    .select(client_addr.as_ref())
                    .unwrap_or_else(|| healthy_upstreams[0].url.clone());
                let forward_path = reconstruct_forward_path(&match_path, &best_match.prefix, &path_variables);
                Some((selected_upstream, forward_path))
            }
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
                .header("traceparent", &traceparent)
                .header("x-trace-id", &trace_short)
                .body(Body::from(format!("{{\"error\":\"No upstream configured for path: {}\"}}", match_path)))
                .unwrap();
        }
    };

    info!("→ {} {} -> {} [trace={}]", req_method, match_path, upstream, trace_short);

    // WebSocket/gRPC 分支日志时间记录
    let start = Instant::now();

    // 如果是 WebSocket 请求，走 WebSocket 代理逻辑
    if is_websocket {
        // WebSocket 需要保留完整路径，如果 forward_path 为空则使用原始路径
        let ws_path = if forward_path.is_empty() {
            match_path.to_string()
        } else {
            forward_path
        };

        // 从 auth 中间件注入的 X-User-Id 获取 uid
        let uid = req.headers()
            .get("X-User-Id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("0")
            .to_string();

        // 构建上游查询参数：注入 uid，保留 clientId，剥离 token
        let mut ws_params: Vec<String> = vec![format!("uid={}", uid)];
        if let Some(raw_query) = req.uri().query() {
            for pair in raw_query.split('&') {
                let key = pair.split('=').next().unwrap_or("");
                // 保留 clientId 等非 token 参数
                if key != "token" {
                    ws_params.push(pair.to_string());
                }
            }
        }
        let ws_query = format!("?{}", ws_params.join("&"));

        let ws_url = format!("{}{}{}",
            upstream.replace("http://", "ws://").replace("https://", "wss://"),
            ws_path,
            ws_query
        );
        info!("检测到 WebSocket 请求，转发到: {}", ws_url);
        return crate::websocket::handle_websocket(req, ws_url).await;
    }

    let mut rb = HTTP_CLIENT
        .request(req_method.clone(), format!("{}{}{}", upstream, forward_path, query_suffix));

    // 设置超时
    if let Some(s) = &settings {
        rb = rb.timeout(s.request_timeout());
    }

    // 注入 traceparent 和 x-request-id（分布式链路追踪）
    rb = rb.header("traceparent", &traceparent);
    rb = rb.header("x-request-id", &trace_id);

    // 复制 headers（跳过 host 和已注入的 trace headers）
    for (name, value) in req.headers().iter() {
        if name == &axum::http::header::HOST { continue; }
        rb = rb.header(name, value);
    }

    // 流式转发请求体（避免全量缓冲到内存）
    let body_stream = req.into_body().into_data_stream();
    let resp_result = rb
        .body(reqwest::Body::wrap_stream(body_stream))
        .send()
        .await;

    match resp_result {
        Ok(resp) => {
            let status = resp.status();
            let headers = resp.headers().clone();
            let duration_ms = start.elapsed().as_millis();

            // 记录响应日志
            if status.is_server_error() {
                tracing::error!("← {} {} {} {}ms [trace={}]", status.as_u16(), req_method, match_path, duration_ms, trace_short);
            } else if status.is_client_error() {
                tracing::warn!("← {} {} {} {}ms [trace={}]", status.as_u16(), req_method, match_path, duration_ms, trace_short);
            } else {
                tracing::info!("← {} {} {} {}ms [trace={}]", status.as_u16(), req_method, match_path, duration_ms, trace_short);
            }

            // 多级慢请求告警
            if let Some(level) = slow_request_level(duration_ms) {
                tracing::warn!("{} {}ms {} {} [trace={}]", level, duration_ms, req_method, match_path, trace_short);
            }

            let mut builder = Response::builder().status(status);

            // 转发响应头
            for (name, value) in headers.iter() {
                builder = builder.header(name, value);
            }

            // 注入 trace headers 到响应
            builder = builder.header("traceparent", &traceparent);
            builder = builder.header("x-trace-id", &trace_short);

            // 兜底 Content-Type
            if !builder.headers_ref().map(|h| h.contains_key(axum::http::header::CONTENT_TYPE)).unwrap_or(false) {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/octet-stream");
            }

            // 流式转发响应体（避免全量缓冲到内存）
            let body_stream = resp.bytes_stream();
            builder.body(Body::from_stream(body_stream)).unwrap()
        }
        Err(err) => {
            let duration_ms = start.elapsed().as_millis();
            tracing::error!("← 502 {} {} {}ms [trace={}] error={}", req_method, match_path, duration_ms, trace_short, err);
            Response::builder()
                .status(502)
                .header(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")
                .header("traceparent", &traceparent)
                .header("x-trace-id", &trace_short)
                .body(Body::from(format!("{{\"error\":\"Proxy error: {}\"}}", err)))
                .unwrap()
        }
    }
}

// ===== 获取或创建负载均衡器（路由粒度复用，上游变化时原地更新） =====
fn get_or_create_balancer(
    route_prefix: &[String],
    upstreams: &[WeightedUpstream],
    strategy: &str,
) -> Arc<dyn LoadBalancer + Send + Sync> {
    // 用路由前缀 + 策略名称作为稳定的 key，而不是 upstreams
    let key = format!("{}:{}", strategy, route_prefix.join(","));
    let urls: Vec<String> = upstreams.iter().map(|u| u.url.clone()).collect();
    let balancer = BALANCERS
        .entry(key)
        .or_insert_with(|| {
            match strategy {
                "random" => Arc::new(WeightedRandomBalancer::new(upstreams.to_vec())),
                "iphash" => Arc::new(IpHashBalancer::new(urls.clone())),
                _ => Arc::new(RoundRobinBalancer::new(urls.clone())), // 默认轮询
            }
        });

    // 每次都更新上游列表（带权重），以便 Nacos/配置文件变更后即时生效
    balancer.update_upstreams(upstreams.to_vec());
    balancer.clone()
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

    if let Some(shared) = req.extensions().get::<crate::config::SharedRouteRules>() {
        let rules = shared.load();
        if let Some(rule) = find_best_match(&rules, match_path) {
            tracing::debug!("找到匹配规则: prefix={:?}, whitelist={:?}", rule.prefix, rule.whitelist);
            // 使用预编译的白名单模式匹配（无锁）
            if rule.is_whitelist_hit(match_path) {
                tracing::info!("✓ 白名单命中，跳过鉴权: {}", match_path);
                req.extensions_mut().insert(WhitelistBypass);
            } else if rule.whitelist.is_some() {
                tracing::warn!("✗ 白名单未命中: {}", match_path);
            }
        } else {
            tracing::warn!("未找到匹配的路由规则: {}", match_path);
        }
    }

    next.run(req).await
}

// ===== 透传租户和用户信息中间件 =====
/// 合并鉴权 + header 透传中间件
/// JWT 解码 → 提取 Claims → 注入 X-User-Id / X-Tenant-Id / X-Username
async fn auth_and_propagate(mut req: Request<Body>, next: Next) -> Response<Body> {
    use crate::auth::{Claims, AuthError};
    use jsonwebtoken::{decode, Algorithm, Validation, TokenData};

    // 先剥离外部可能伪造的 X-User-* headers（安全防护）
    req.headers_mut().remove("X-User-Id");
    req.headers_mut().remove("X-Tenant-Id");
    req.headers_mut().remove("X-Username");

    // 白名单标记则跳过鉴权，直接放行
    if req.extensions().get::<WhitelistBypass>().is_some() {
        return next.run(req).await;
    }

    // 获取 DecodingKey
    let decoding_key = match req.extensions().get::<Arc<jsonwebtoken::DecodingKey>>() {
        Some(k) => k.clone(),
        None => {
            return AuthError::ConfigMissing.into_response();
        }
    };

    // 提取 JWT token：优先 Authorization header，WS 回退到 ?token= 查询参数
    let auth_header = req.headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.to_string());

    let token = if let Some(ref header) = auth_header {
        if !header.starts_with("Bearer ") {
            return AuthError::InvalidToken.into_response();
        }
        header.trim_start_matches("Bearer ").trim().to_string()
    } else {
        // WebSocket 不支持自定义 header，从 ?token= 查询参数提取
        match req.uri().query()
            .and_then(|q| q.split('&').find(|p| p.starts_with("token=")))
            .and_then(|p| p.strip_prefix("token="))
        {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => return AuthError::MissingHeader.into_response(),
        }
    };

    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    let token_data: TokenData<Claims> = match decode(&token, &decoding_key, &validation) {
        Ok(td) => td,
        Err(e) => {
            return AuthError::DecodeError(e).into_response();
        }
    };

    let claims = token_data.claims;

    // 注入网关验证过的用户信息 headers
    if !claims.sub.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&claims.sub) {
            req.headers_mut().insert("X-User-Id", v);
        }
    }
    let tenant_id = claims.tenant_id();
    if !tenant_id.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&tenant_id) {
            req.headers_mut().insert("X-Tenant-Id", v);
        }
    }
    let username = claims.username();
    if !username.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&username) {
            req.headers_mut().insert("X-Username", v);
        }
    }

    next.run(req).await
}
