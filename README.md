# ms-gateway

一个基于 Rust 和 Axum 的高性能 API 网关，支持负载均衡、JWT 认证、限流和白名单功能。

## 功能特性

### 🚀 核心功能
- **智能路由匹配**: 支持 Spring Gateway 风格的路径匹配模式
  - `*` - 单级通配符
  - `**` - 多级通配符  
  - `?` - 单字符匹配
  - `{variable}` - 路径变量
  - `{variable:regex}` - 带正则约束的路径变量

- **多种负载均衡策略**
  - 轮询 (Round Robin)
  - 加权随机 (Weighted Random)
  - IP 哈希 (IP Hash) - 支持一致性哈希

- **JWT 认证与授权**
  - 自动解析和验证 JWT Token
  - 支持多租户 (tenant_id)
  - 用户信息透传到上游服务

- **白名单机制**
  - 支持路径白名单，跳过 JWT 验证
  - 灵活的路径匹配规则

- **限流保护**
  - 全局 QPS 限制
  - 客户端级别限流
  - 基于令牌桶算法

### 🛠 技术特性
- **高性能**: 基于 Rust 和 Tokio 异步运行时
- **零拷贝**: 高效的请求转发机制
- **动态配置**: 支持热重载路由规则
- **监控友好**: 集成 Prometheus 指标
- **容器化**: 支持 Docker 部署

## 快速开始

### 环境要求
- Rust 1.70+
- 上游服务 (用于测试)

### 安装与运行

1. **克隆项目**
```bash
git clone <repository-url>
cd rust-gateway
```

2. **配置环境变量**
```bash
# 创建 .env 文件
cp .env.example .env

# 编辑配置
GATEWAY_BIND=0.0.0.0:8080
JWT_DECODING_KEY=your-secret-key
GLOBAL_QPS=10000
CLIENT_QPS=1000
```

3. **配置路由规则**
编辑 `routes.toml`:
```toml
[[routes]]
prefix = ["/api/**"]
upstream = ["http://localhost:3000", "http://localhost:3001"]
strategy = "robin"
whitelist = ["/api/health", "/api/status"]

[[routes]]
prefix = ["/user/{id}"]
upstream = "http://localhost:3002"
strategy = "iphash"
```

4. **启动服务**
```bash
# 开发模式
cargo run

# 生产模式
cargo build --release
./target/release/rust-gateway
```

### 测试服务

项目包含测试用的上游服务，可以同时启动多个实例：

```bash
# 启动测试服务 (端口 30000, 30001, 30002)
cargo run --bin service_30000
cargo run --bin service_30001  
cargo run --bin service_30002
```

## 配置说明

### 主配置文件 (config.toml 或环境变量)

| 配置项 | 说明 | 默认值 |
|--------|------|--------|
| `gateway_bind` | 网关监听地址 | `0.0.0.0:8080` |
| `jwt_decoding_key` | JWT 解码密钥 | `dev-secret` |
| `global_qps` | 全局 QPS 限制 | `10000` |
| `client_qps` | 单客户端 QPS 限制 | `1000` |
| `request_timeout_secs` | 请求超时时间(秒) | `10` |

### 路由配置 (routes.toml)

```toml
[[routes]]
# 路径前缀，支持字符串或数组
prefix = ["/api/**", "/v1/**"]

# 上游服务，支持字符串或数组
upstream = ["http://service1:8080", "http://service2:8080"]

# 负载均衡策略: robin, random, iphash
strategy = "robin"

# 白名单路径，命中则跳过 JWT 验证
whitelist = ["/api/health", "/api/metrics"]
```

## 负载均衡策略

### 1. 轮询 (robin)
- 按顺序轮流分发请求
- 适合服务实例性能相近的场景

### 2. 加权随机 (random)  
- 根据权重随机选择服务实例
- 支持动态调整权重

### 3. IP 哈希 (iphash)
- 基于客户端 IP 的一致性哈希
- 确保同一客户端总是访问同一服务实例
- 支持服务实例动态变化

## API 使用示例

### 1. 带认证的请求
```bash
curl -H "Authorization: Bearer <jwt-token>" \
     http://localhost:8080/proxy/api/users
```

### 2. 白名单请求 (无需认证)
```bash
curl http://localhost:8080/proxy/api/health
```

### 3. 路径变量匹配
```bash
curl http://localhost:8080/proxy/user/123
# 匹配规则: /user/{id}
```

## 监控与指标

网关集成了 Prometheus 指标，可通过以下端点查看：

```bash
curl http://localhost:8080/metrics
```

主要指标包括：
- 请求总数和错误率
- 响应时间分布
- 负载均衡器状态
- 限流统计

## 开发指南

### 项目结构
```
src/
├── main.rs              # 主入口
├── config.rs            # 配置管理
├── proxy.rs             # 代理逻辑
├── auth.rs              # JWT 认证
├── rate_limit.rs        # 限流实现
├── metrics.rs           # 监控指标
├── path_matcher.rs      # 路径匹配
└── load_balancer/       # 负载均衡器
    ├── mod.rs
    ├── round_robin.rs
    ├── weighted_random.rs
    └── ip_hash.rs
```

### 添加新的负载均衡策略

1. 在 `src/load_balancer/` 下创建新文件
2. 实现 `LoadBalancer` trait
3. 在 `mod.rs` 中导出
4. 在 `config.rs` 中添加策略支持

### 自定义中间件

```rust
use axum::middleware;

async fn custom_middleware(req: Request<Body>, next: Next) -> Response<Body> {
    // 前置处理
    let response = next.run(req).await;
    // 后置处理
    response
}

// 在路由中使用
.route_layer(middleware::from_fn(custom_middleware))
```

## 部署

### Docker 部署

```dockerfile
FROM rust:1.70 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates
COPY --from=builder /app/target/release/rust-gateway /usr/local/bin/
COPY --from=builder /app/routes.toml /app/
WORKDIR /app
CMD ["rust-gateway"]
```

### 生产环境建议

1. **配置优化**
   - 调整连接池大小
   - 设置合适的超时时间
   - 启用日志轮转

2. **监控告警**
   - 设置 Prometheus 告警规则
   - 监控错误率和响应时间
   - 关注内存和 CPU 使用率

3. **安全加固**
   - 使用强密钥
   - 启用 HTTPS
   - 配置防火墙规则

## 许可证

本项目采用 [Apache 2.0 许可证](LICENSE)。

## 贡献

欢迎提交 Issue 和 Pull Request！

## 更新日志

### v0.1.0
- 初始版本发布
- 支持基础路由和负载均衡
- JWT 认证和白名单功能
- 限流和监控集成
