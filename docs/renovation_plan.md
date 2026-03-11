# ms-gateway 改造计划

> 版本: v1.0 | 日期: 2026-03-11

---

## 一、改造概览

基于架构分析，按优先级分阶段实施改造。高优先级涉及 **功能性 Bug 修复** 和 **性能/稳定性隐患**。

## 二、高优先级改造 🔴

### 2.1 修复 IP Hash 负载均衡失效

**问题**：`proxy_handler` 调用 `balancer.select(None)`，`IpHashBalancer` 始终对 `127.0.0.1` 做哈希，一致性哈希完全失效。

**改造方案**：
- 从 `ConnectInfo<SocketAddr>` 提取真实客户端 IP
- 传入 `balancer.select(Some(&client_addr))`

**涉及文件**：`src/proxy.rs`

---

### 2.2 修复 Prometheus 指标标签爆炸

**问题**：使用完整请求路径（如 `/api/user/123`）作为 Prometheus label，导致时间序列无限增长，最终 Prometheus OOM。

**改造方案**：
- 在 `prometheus_middleware` 中匹配路由模板，使用模板路径（如 `/api/user/{id}`）作为标签
- 未匹配到路由时使用 `"unmatched"` 作为 path 标签

**涉及文件**：`src/metrics.rs`、`src/proxy.rs`

---

### 2.3 路径匹配优化：预编译正则 + 移除 Mutex

**问题**：
1. `PATTERN_CACHE` 使用 `Mutex<HashMap>` 缓存正则，高并发下锁竞争严重
2. 每次路径匹配都走缓存查找，增加不必要的开销

**改造方案**：
- 在 `RouteRule` 结构体中新增 `compiled_patterns: Vec<RoutePattern>` 和 `compiled_whitelist: Vec<RoutePattern>` 字段
- `load_route_rules()` 加载时预编译所有正则
- `matches()` 和白名单检查直接使用预编译结果，不再走缓存
- 保留 `PATTERN_CACHE` 供其他动态场景使用，但主请求路径不再依赖

**涉及文件**：`src/config.rs`、`src/proxy.rs`、`src/path_matcher.rs`

---

### 2.4 请求/响应 Body 流式转发

**问题**：当前将请求体和响应体全量读入内存，大文件时内存爆炸。

**改造方案**：
- 请求 body：将 `axum::body::Body` 转为 `reqwest::Body::wrap_stream()`，流式转发
- 响应 body：将 `reqwest::Response` 的 bytes stream 包装为 `axum::body::Body`，流式返回
- 限制非流式回退的 body 大小上限

**涉及文件**：`src/proxy.rs`

---

## 三、中优先级改造 🟡（后续实施）

| 编号 | 改造项 | 说明 |
|------|--------|------|
| 3.1 | 上游健康检查 | 定期探测上游 `/health`，自动剔除不健康节点 |
| 3.2 | 路由匹配优化 | 用 Trie 树 / HashMap 预筛选替代线性扫描 |
| 3.3 | per-IP 限流清理 | 定期清理不活跃 IP 的令牌桶状态 |
| 3.4 | 优雅关闭 | 实现 `with_graceful_shutdown`，等待在途请求 |
| 3.5 | JWT DecodingKey 缓存 | 启动时预构造，避免每次请求重建 |

## 四、低优先级改造 🟢（后续实施）

| 编号 | 改造项 | 说明 |
|------|--------|------|
| 4.1 | 反序列化器去重 | 合并 `prefix_deserializer` / `upstream_deserializer` |
| 4.2 | WeightedRandom 索引修复 | weight=0 节点过滤后索引对齐 |
| 4.3 | 请求链路追踪 | 添加 `x-request-id` 生成与传播 |
| 4.4 | 路由热重载 | 文件 watcher 或 reload API |
| 4.5 | CORS 安全加固 | 限制为已知域名 |

## 五、验证计划

### 5.1 编译验证
```bash
cargo build 2>&1
```

### 5.2 单元测试
```bash
cargo test 2>&1
```

### 5.3 手动验证（后续集成测试时补充）
- 启动网关 + 上游测试服务，验证路由转发正常
- 验证 `/metrics` 端点的 path 标签为路由模板而非实际路径
- 验证不同 IP 的请求分发到不同上游
