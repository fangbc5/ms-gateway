# WebSocket 支持说明

## 功能概述

网关现已支持 WebSocket 长连接代理，可以透明地转发 WebSocket 连接到上游服务。

## 实现原理

1. **协议检测**：检测 HTTP 请求头中的 `Upgrade: websocket`
2. **连接升级**：将 HTTP 连接升级为 WebSocket 连接
3. **双向转发**：在客户端和上游服务之间建立双向消息通道
4. **自动协议转换**：HTTP/HTTPS 自动转换为 WS/WSS

## 配置示例

在 `routes.yaml` 中配置 WebSocket 路由：

```yaml
routes:
  - prefix:
      - /ws
      - /websocket
    upstream:
      - http://localhost:9000
    strategy: round_robin
```

## 使用方式

### 客户端连接

```javascript
// 通过网关连接到上游 WebSocket 服务
const ws = new WebSocket('ws://gateway:8080/proxy/ws');

ws.onopen = () => console.log('Connected');
ws.onmessage = (event) => console.log('Received:', event.data);
ws.send('Hello Server!');
```

### 测试

1. 启动一个 WebSocket 测试服务（如 `ws://localhost:9000`）
2. 配置网关路由指向该服务
3. 打开 `examples/websocket_test.html` 进行测试

## 特性

- ✅ 支持 WebSocket 长连接
- ✅ 双向消息转发
- ✅ 自动协议升级
- ✅ 支持负载均衡
- ✅ 支持路由匹配
- ✅ 支持认证和限流（通过中间件）

## 注意事项

1. WebSocket 连接会绕过 JWT 认证，如需认证请在白名单中配置
2. 确保上游服务支持 WebSocket 协议
3. 长连接会占用资源，建议配置合理的超时和连接数限制
