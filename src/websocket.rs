use axum::{
    body::Body,
    extract::Request,
    http::{Response, StatusCode},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

/// WebSocket 代理处理器
pub async fn handle_websocket(req: Request<Body>, upstream_url: String) -> Response<Body> {
    info!("开始处理 WebSocket 请求，目标: {}", upstream_url);

    // 检查是否是 WebSocket 升级请求
    let is_upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if !is_upgrade {
        error!("不是 WebSocket 升级请求");
        return (StatusCode::BAD_REQUEST, "Not a WebSocket upgrade request")
            .into_response()
            .map(Body::new);
    }

    // 提取 Sec-WebSocket-Key 用于生成响应
    let ws_key = req
        .headers()
        .get("Sec-WebSocket-Key")
        .and_then(|v| v.to_str().ok())
        .map(|key| derive_accept_key(key))
        .unwrap_or_default();

    info!("准备升级连接，Sec-WebSocket-Accept: {}", ws_key);

    // 创建升级 future（不等待）
    let upgrade_future = hyper::upgrade::on(req);

    // 在后台任务中处理升级和代理
    tokio::spawn(async move {
        match upgrade_future.await {
            Ok(upgraded) => {
                info!("连接升级成功");
                // 使用 TokioIo 包装 upgraded 连接
                let io = TokioIo::new(upgraded);
                let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    io,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    None,
                )
                .await;

                if let Err(e) = proxy_websocket(ws, upstream_url).await {
                    error!("WebSocket proxy error: {}", e);
                }
            }
            Err(e) => {
                error!("Failed to upgrade connection: {}", e);
            }
        }
    });

    // 立即返回 101 Switching Protocols 响应
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(axum::http::header::UPGRADE, "websocket")
        .header(axum::http::header::CONNECTION, "Upgrade")
        .header("Sec-WebSocket-Accept", ws_key)
        .body(Body::empty())
        .unwrap()
}

/// 双向转发 WebSocket 消息
async fn proxy_websocket(
    client_ws: tokio_tungstenite::WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
    upstream_url: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("Connecting to upstream WebSocket: {}", upstream_url);

    // 连接到上游 WebSocket
    let (upstream_ws, _) = connect_async(&upstream_url).await?;
    info!("Connected to upstream WebSocket: {}", upstream_url);

    let (mut client_sink, mut client_stream) = client_ws.split();
    let (mut upstream_sink, mut upstream_stream) = upstream_ws.split();

    // 双向转发
    let client_to_upstream = async {
        while let Some(msg) = client_stream.next().await {
            match msg {
                Ok(msg) => {
                    if let Err(e) = upstream_sink.send(msg).await {
                        warn!("Failed to send to upstream: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    warn!("Client stream error: {}", e);
                    break;
                }
            }
        }
    };

    let upstream_to_client = async {
        while let Some(msg) = upstream_stream.next().await {
            match msg {
                Ok(msg) => {
                    if let Err(e) = client_sink.send(msg).await {
                        warn!("Failed to send to client: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    warn!("Upstream stream error: {}", e);
                    break;
                }
            }
        }
    };

    // 并发执行双向转发，任意一方断开则结束
    tokio::select! {
        _ = client_to_upstream => info!("Client to upstream closed"),
        _ = upstream_to_client => info!("Upstream to client closed"),
    }

    Ok(())
}

/// 计算 Sec-WebSocket-Accept
fn derive_accept_key(key: &str) -> String {
    use base64::Engine;
    use sha1::{Digest, Sha1};
    const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}
