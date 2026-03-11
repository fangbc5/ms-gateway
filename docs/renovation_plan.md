# ms-gateway 改造计划

> 版本: v3.3 | 更新日期: 2026-03-11

---

## 一、改造概览

基于架构分析，按优先级分阶段实施改造。高优先级涉及 **功能性 Bug 修复** 和 **性能/稳定性隐患**。

## 二、高优先级改造 🔴（已全部完成 ✅）

### 2.1 ✅ 修复 IP Hash 负载均衡失效
从 `ConnectInfo<SocketAddr>` 提取真实客户端 IP，传入 `balancer.select(Some(&client_addr))`。
**涉及文件**：`src/proxy.rs`

### 2.2 ✅ 修复 Prometheus 指标标签爆炸
新增 `resolve_route_template()` 函数，使用路由模板路径作为 Prometheus label。
**涉及文件**：`src/metrics.rs`

### 2.3 ✅ 路径匹配优化：预编译正则 + 移除 Mutex
在 `RouteRule` 中预编译正则模式，消除运行时 `Mutex` 竞争。
**涉及文件**：`src/config.rs`、`src/proxy.rs`、`src/path_matcher.rs`

### 2.4 ✅ 请求/响应 Body 流式转发
使用 `Body::from_stream()` 流式转发，避免全量缓冲。
**涉及文件**：`src/proxy.rs`

---

## 三、中优先级改造 🟡（已全部完成 ✅）

| # | 改造项 | 涉及文件 | 状态 |
|---|--------|----------|------|
| 3.1 | 优雅关闭 (Ctrl+C + SIGTERM) | `src/main.rs` | ✅ |
| 3.2 | JWT DecodingKey 预构造 | `src/main.rs`, `src/auth.rs` | ✅ |
| 3.3 | 健康检查 `GET /health` | `src/main.rs` | ✅ |
| 3.4 | CORS 配置化 | `src/config.rs`, `src/main.rs`, `.env.example` | ✅ |
| 3.5 | WeightedRandom 索引修复 | `src/load_balancer/weighted_random.rs` | ✅ |
| 3.6 | 生产构建优化 (LTO/strip) | `Cargo.toml` | ✅ |

---

## 四、低优先级改造 🟢

### 4.1 ✅ 反序列化器去重
合并 `prefix_deserializer` / `upstream_deserializer` 为统一的 `string_or_vec_deser` 模块。
**涉及文件**：`src/config.rs`

### 4.2 ✅ 请求链路追踪 (x-request-id)
新增 `request_id_middleware`：优先使用请求携带的 `x-request-id`，否则生成 UUID v4。
**涉及文件**：`src/main.rs` | **新增依赖**：`uuid` (v4, fast-rng)

### 4.3 ✅ 路由热重载
使用 `ArcSwap` + `notify` crate 实现无锁路由规则热重载，支持文件监听和 `POST /_reload` 手动刷新。
**涉及文件**：`src/config.rs`、`src/main.rs`、`src/proxy.rs`、`src/metrics.rs` | **新增依赖**：`notify` v6

### 4.4 ✅ 上游健康检查（剔除 + 恢复）
后台 tokio task 定期探测上游 `/health`：
- 连续失败 ≥ `unhealthy_threshold` → 标记不健康，负载均衡跳过
- 连续成功 ≥ `healthy_threshold` → 恢复健康
- 全部不健康时降级保护，路由热重载后新上游自动纳入
**涉及文件**：`src/health_check.rs`（新增）、`src/config.rs`、`src/main.rs`、`src/proxy.rs`、`routes.toml`

### 4.5 ✅ Nacos 集成（配置中心 + 服务发现）
可插拔设计，`NACOS_ENABLED=false` 时零影响。支持：
- 从 Nacos 读取路由规则（优先级：环境变量 > Nacos > routes.toml）
- Nacos 配置热更新（ConfigListener）
- 服务发现（`service_name` 路由字段 + InstanceListener）
- 网关自注册
**涉及文件**：`src/nacos/`（新增 4 文件）、`src/config.rs`、`src/proxy.rs`、`src/main.rs`、`Cargo.toml`、`.env.example`

### 4.6 ✅ main.rs 重构
提取 `init_tracing()`、`build_cors()`、`build_router()`、`start_server()` 独立函数，`main()` 精简至 ~20 行。

### 4.7 ⬜ 路由匹配优化
用 Trie 树 / HashMap 预筛选替代线性扫描。

### 4.8 ⬜ per-IP 限流清理
governor DefaultKeyedStateStore 无清理 API，需评估替代方案。

---

## 五、验证结果

| 验证项 | 结果 |
|--------|------|
| `cargo build` | ✅ 编译通过 |
| `cargo test` | ✅ 33 passed, 0 failed |
| 手动启动 + Ctrl+C | ✅ 优雅关闭正常 |
