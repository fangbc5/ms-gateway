# ms-gateway Nacos 集成设计方案

> 版本: v1.1 | 日期: 2026-03-11

---

## 一、设计目标

为 ms-gateway 引入**可插拔**的 Nacos 支持，通过 `.env` 配置开关控制：

1. **配置中心**：从 Nacos 读取网关配置（Settings + 路由规则），优先于本地文件
2. **配置热更新**：监听 Nacos 配置变更，自动重载路由规则
3. **服务发现**：路由上游支持 Nacos 服务名，自动发现并动态更新实例列表
4. **网关自注册**：可选将网关自身注册到 Nacos

> **核心原则**：`NACOS_ENABLED=false`（默认）时，Nacos 模块不初始化、不连接，行为与现有完全一致。

---

## 二、配置设计

### 2.1 新增环境变量 (.env)

```env
# ========== Nacos 集成（可选，默认关闭） ==========

# 总开关
NACOS_ENABLED=false
NACOS_SERVER_ADDRS=127.0.0.1:8848
NACOS_NAMESPACE=public
NACOS_USERNAME=nacos
NACOS_PASSWORD=nacos
NACOS_GROUP=DEFAULT_GROUP

# 网关自注册（可选）
NACOS_REGISTER_ENABLED=false
NACOS_SERVICE_NAME=ms-gateway

# 从 Nacos 读取网关主配置（优先于本地 .env）
NACOS_CONFIG_DATA_ID=ms-gateway
NACOS_CONFIG_GROUP=DEFAULT_GROUP

# 从 Nacos 读取路由规则（优先于本地 routes.toml）
NACOS_ROUTES_DATA_ID=ms-gateway-routes
NACOS_ROUTES_GROUP=DEFAULT_GROUP
```

### 2.2 NacosSettings 结构

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct NacosSettings {
    #[serde(default)]
    pub enabled: bool,                       // 总开关，默认 false
    #[serde(default = "default_server_addrs")]
    pub server_addrs: String,                // "127.0.0.1:8848"（逗号分隔多地址）
    pub namespace: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "default_group")]
    pub group: String,                       // DEFAULT_GROUP

    // 网关自注册
    #[serde(default)]
    pub register_enabled: bool,
    #[serde(default)]
    pub service_name: Option<String>,        // 注册到 Nacos 的服务名

    // 配置中心
    pub config_data_id: Option<String>,      // 网关主配置的 data_id
    pub config_group: Option<String>,
    pub routes_data_id: Option<String>,      // 路由规则的 data_id
    pub routes_group: Option<String>,
}
```

在 `Settings` 中增加：

```rust
pub struct Settings {
    // ... 现有字段 ...
    #[serde(default)]
    pub nacos: Option<NacosSettings>,
}
```

### 2.3 配置优先级

```
本地 .env / 环境变量 (最高优先级)
       ↓
  Nacos 配置中心
       ↓
  本地 routes.toml (最低)
```

---

## 三、模块设计

```
src/
├── nacos/
│   ├── mod.rs          # 模块入口，共享状态（DashMap）
│   ├── client.rs       # NamingClient / ConfigClient 初始化
│   ├── config.rs       # 配置拉取 + 变更监听
│   └── discovery.rs    # 服务发现 + 实例订阅
├── config.rs           # 增加 NacosSettings，load 逻辑
├── proxy.rs            # upstream 支持服务名查表
└── main.rs             # 条件启动 Nacos
```

### 3.1 路由扩展：服务名发现

```toml
# 传统方式（保持兼容）
[[routes]]
prefix = ["/auth/**"]
upstream = "http://host.docker.internal:30000"

# Nacos 服务发现方式（新增）
[[routes]]
prefix = ["/user/**"]
service_name = "ms-user-service"
strategy = "round_robin"
```

`service_name` 非空时，从 Nacos 实例列表动态构建 upstream（DashMap 查表）。

### 3.2 配置热更新

- 路由配置变更 → `ConfigListener.change()` 回调 → 反序列化 TOML → `SharedRouteRules.store()` 原子替换
- 复用现有 ArcSwap 热重载机制，与 `notify` 文件监听共存

### 3.3 服务发现热更新

- `InstanceListener.change()` 回调 → 更新 `DashMap<service_name, Vec<Instance>>`
- `proxy_handler` 选择上游时查表获取最新实例列表

### 3.4 启动流程

```
main() {
    1. load_settings()             // 本地 .env
    2. if nacos.enabled {
        a. init_nacos()            // 初始化客户端
        b. fetch_and_merge_config()// 拉取配置
        c. subscribe_config()      // 订阅配置变更
        d. subscribe_services()    // 订阅服务发现
        e. if register_enabled { register_self() }
    }
    3. load_route_rules()          // 可能已被 Nacos 覆盖
    4. start_health_checker()
    5. start_server()
}
```

---

## 四、依赖

参照 fbc-starter，使用 `nacos_rust_client` v0.3（直接加入 `[dependencies]`，无需 feature gate）：

```toml
[dependencies]
nacos_rust_client = "0.3"
```

关键 API：

| API | 用途 |
|-----|------|
| `ClientBuilder::new().set_endpoint_addrs().build()` | 初始化客户端 |
| `NamingClient::register(instance)` / `unregister()` | 注册/注销服务 |
| `NamingClient::subscribe(listener)` | 订阅实例变更 |
| `ConfigClient::subscribe(listener)` | 订阅配置变更 |

---

## 五、涉及文件

| 文件 | 操作 | 说明 |
|------|------|------|
| `Cargo.toml` | MODIFY | 增加 `nacos_rust_client` 依赖 |
| `src/nacos/mod.rs` | NEW | 模块入口，DashMap 存储 |
| `src/nacos/client.rs` | NEW | 客户端初始化 |
| `src/nacos/config.rs` | NEW | 配置拉取 + 监听 |
| `src/nacos/discovery.rs` | NEW | 服务发现 + 实例订阅 |
| `src/config.rs` | MODIFY | NacosSettings + load 逻辑 |
| `src/proxy.rs` | MODIFY | 支持 service_name 上游查表 |
| `src/main.rs` | MODIFY | 条件启动 Nacos |
| `.env.example` | MODIFY | 增加 Nacos 配置示例 |

## 六、验证计划

### 自动化测试
- `cargo build` / `cargo test` — 现有测试全部通过
- Nacos 模块单元测试（NacosSettings 反序列化、默认值）

### 手动验证
- 启动 rNacos（参考 fbc-starter docker-compose-rnacos.yml）
- Nacos 中配置路由规则 → 网关读取 → 代理正常
- 修改 Nacos 配置 → 网关热更新
- 上游服务注册到 Nacos → 网关自动发现
