use config::{Config, ConfigError, File};
use serde::Deserialize;
use std::{env, path::PathBuf, time::Duration};
use crate::path_matcher::RoutePattern;
use crate::health_check::HealthCheckConfig;
use std::collections::HashMap;
use arc_swap::ArcSwap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 共享路由规则：使用 ArcSwap 实现无锁热重载
pub type SharedRouteRules = Arc<ArcSwap<Vec<RouteRule>>>;

/// 标记 Nacos 是否正在管理路由配置
/// 当 Nacos 路由激活时，本地 routes.toml 文件变更将被忽略，避免配置冲突
static NACOS_ROUTES_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 设置 Nacos 路由激活状态
pub fn set_nacos_routes_active(active: bool) {
    NACOS_ROUTES_ACTIVE.store(active, Ordering::SeqCst);
    if active {
        tracing::info!("🔒 Nacos 路由已激活，本地 routes.toml 变更将被忽略");
    } else {
        tracing::info!("🔓 Nacos 路由已停用，恢复本地 routes.toml 管理");
    }
}

/// 检查 Nacos 路由是否激活
pub fn is_nacos_routes_active() -> bool {
    NACOS_ROUTES_ACTIVE.load(Ordering::SeqCst)
}

#[derive(Debug, Deserialize, Clone)]
pub struct RouteRule {
    // 支持单个或多个前缀
    #[serde(with = "string_or_vec_deser")]
    pub prefix: Vec<String>,
    // 支持单个或多个上游（和 service_name 二选一）
    #[serde(default, with = "string_or_vec_deser")]
    pub upstream: Vec<String>,
    // Nacos 服务名（和 upstream 二选一，启用 Nacos 时从注册中心发现实例）
    // 同时支持 service_name 和 server_name 两种写法
    #[serde(default, alias = "server_name")]
    pub service_name: Option<String>,
    // 负载均衡策略，默认为轮询
    #[serde(default = "default_strategy")]
    pub strategy: String,
    // 白名单路径（命中则跳过鉴权），支持 string 或 array
    #[serde(default, deserialize_with = "opt_vec_string_deser::deserialize")] 
    pub whitelist: Option<Vec<String>>,
    // ===== 预编译字段（启动时填充，避免运行时 Mutex 竞争）=====
    /// 预编译的前缀匹配模式
    #[serde(skip)]
    pub compiled_prefixes: Vec<RoutePattern>,
    /// 预编译的白名单匹配模式
    #[serde(skip)]
    pub compiled_whitelist: Vec<RoutePattern>,
}

// 默认负载均衡策略
fn default_strategy() -> String {
    "robin".to_string()
}

// 通用反序列化器：支持字符串和数组两种格式（prefix 和 upstream 共用）
mod string_or_vec_deser {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StringOrVec {
            String(String),
            Vec(Vec<String>),
        }

        match StringOrVec::deserialize(deserializer)? {
            StringOrVec::String(s) => Ok(vec![s]),
            StringOrVec::Vec(v) => Ok(v),
        }
    }
}

// 反序列化 Option<Vec<String>>，既支持缺省(None)，也支持 string 或 array
mod opt_vec_string_deser {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OptStringOrVec {
            None,
            String(String),
            Vec(Vec<String>),
        }
        let v = Option::<OptStringOrVec>::deserialize(deserializer)?;
        Ok(match v {
            Some(OptStringOrVec::String(s)) => Some(vec![s]),
            Some(OptStringOrVec::Vec(v)) => Some(v),
            _ => None,
        })
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub gateway_bind: String,
    pub jwt_decoding_key: String,
    pub global_qps: u32,
    pub client_qps: u32,
    pub request_timeout_secs: Option<u64>,
    /// CORS 允许的源列表（逗号分隔），留空或不配置则允许所有
    #[serde(default)]
    pub cors_allowed_origins: Option<String>,
    /// Nacos 配置（可选，默认不启用）
    #[serde(default)]
    pub nacos: Option<NacosSettings>,
}

/// Nacos 集成配置
#[derive(Debug, Deserialize, Clone)]
pub struct NacosSettings {
    /// 总开关，默认 false
    #[serde(default)]
    pub enabled: bool,
    /// Nacos 服务器地址（逗号分隔多地址）
    #[serde(default = "default_nacos_addrs")]
    pub server_addrs: String,
    /// 认证用户名
    pub username: Option<String>,
    /// 认证密码
    pub password: Option<String>,

    // ---- 配置中心（Config Center）命名空间 & 组 ----
    /// 配置中心命名空间（独立于服务注册）
    #[serde(default = "default_nacos_namespace")]
    pub config_namespace: String,
    /// 配置中心默认组
    #[serde(default = "default_nacos_group")]
    pub config_group: String,

    // ---- 服务注册/发现（Naming）命名空间 & 组 ----
    /// 服务注册/发现命名空间（独立于配置中心）
    #[serde(default = "default_nacos_namespace")]
    pub naming_namespace: String,
    /// 服务注册/发现默认组
    #[serde(default = "default_nacos_group")]
    pub naming_group: String,

    /// 是否注册网关自身
    #[serde(default)]
    pub register_enabled: bool,
    /// 注册到 Nacos 的服务名
    pub service_name: Option<String>,

    /// 路由规则的 data_id（从 Nacos 读取 routes）
    pub routes_data_id: Option<String>,
    /// 路由规则的 group（可选，默认使用 config_group）
    pub routes_group: Option<String>,
}

fn default_nacos_addrs() -> String { "127.0.0.1:8848".to_string() }
fn default_nacos_namespace() -> String { "public".to_string() }
fn default_nacos_group() -> String { "DEFAULT_GROUP".to_string() }

impl Settings {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs.unwrap_or(10))
    }
}

// 增强的路径匹配器
impl RouteRule {
    /// 预编译所有前缀和白名单的正则模式（启动时调用一次）
    pub fn compile_patterns(&mut self) {
        // 预编译前缀
        self.compiled_prefixes = self.prefix.iter()
            .filter_map(|p| {
                if p.contains('{') || p.contains('*') || p.contains('?') {
                    RoutePattern::from_pattern(p).ok()
                } else {
                    None
                }
            })
            .collect();

        // 预编译白名单
        if let Some(whitelist) = &self.whitelist {
            self.compiled_whitelist = whitelist.iter()
                .filter_map(|w| {
                    if w.contains('{') || w.contains('*') || w.contains('?') {
                        RoutePattern::from_pattern(w).ok()
                    } else {
                        None
                    }
                })
                .collect();
        }
    }

    pub fn matches(&self, path: &str) -> bool {
        for (i, prefix) in self.prefix.iter().enumerate() {
            if self.matches_prefix_compiled(prefix, path, i) {
                return true;
            }
        }
        false
    }

    fn matches_prefix_compiled(&self, prefix: &str, path: &str, _prefix_idx: usize) -> bool {
        if prefix.contains('{') || prefix.contains('*') || prefix.contains('?') {
            // 优先使用预编译的模式（无锁）
            if let Some(compiled) = self.compiled_prefixes.iter().find(|rp| rp.pattern() == prefix) {
                return compiled.matches(path);
            }
            // 回退到缓存（极少触发）
            match RoutePattern::from_pattern(prefix) {
                Ok(route_pattern) => route_pattern.matches(path),
                Err(_) => path.starts_with(prefix),
            }
        } else {
            path == prefix || path.starts_with(&format!("{}/", prefix))
        }
    }

    /// 检查路径是否命中白名单（使用预编译模式，无锁）
    pub fn is_whitelist_hit(&self, path: &str) -> bool {
        if let Some(whitelist) = &self.whitelist {
            for w in whitelist {
                let matched = if w.contains('{') || w.contains('*') || w.contains('?') {
                    // 优先使用预编译的白名单模式
                    if let Some(compiled) = self.compiled_whitelist.iter().find(|rp| rp.pattern() == w) {
                        compiled.matches(path)
                    } else {
                        RoutePattern::from_pattern(w)
                            .map(|rp| rp.matches(path))
                            .unwrap_or(false)
                    }
                } else {
                    path == w || path.starts_with(&format!("{}/", w))
                };
                if matched {
                    return true;
                }
            }
        }
        false
    }

    pub fn extract_variables(&self, path: &str) -> HashMap<String, String> {
        for (i, prefix) in self.prefix.iter().enumerate() {
            if self.matches_prefix_compiled(prefix, path, i) {
                // 优先使用预编译模式
                if let Some(compiled) = self.compiled_prefixes.iter().find(|rp| rp.pattern() == prefix) {
                    return compiled.match_path(path).unwrap_or_default();
                }
                match RoutePattern::from_pattern(prefix) {
                    Ok(route_pattern) => return route_pattern.match_path(path).unwrap_or_default(),
                    Err(_) => return HashMap::new(),
                }
            }
        }
        HashMap::new()
    }

    // 校验配置
    pub fn validate(&self) -> Result<(), String> {
        if self.prefix.is_empty() {
            return Err("prefix不能为空".to_string());
        }
        for (i, p) in self.prefix.iter().enumerate() {
            if p.trim().is_empty() {
                return Err(format!("prefix[{}]不能为空", i));
            }
        }
        // upstream 和 service_name 至少配置一个
        if self.upstream.is_empty() && self.service_name.is_none() {
            return Err("upstream 和 service_name 不能同时为空".to_string());
        }
        for (i, u) in self.upstream.iter().enumerate() {
            if u.trim().is_empty() {
                return Err(format!("upstream[{}]不能为空", i));
            }
        }
        
        // 校验负载均衡策略
        match self.strategy.as_str() {
            "robin" | "random" | "iphash" => Ok(()),
            _ => Err(format!("不支持的负载均衡策略: {}", self.strategy)),
        }
    }
}

pub fn load_settings() -> Result<Settings, config::ConfigError> {
    // 先加载环境变量
    dotenvy::dotenv().ok();

    let builder = Config::builder()
        .add_source(File::with_name("config").required(false))
        .add_source(
            config::Environment::default()
                .separator("__")  // 支持嵌套结构: NACOS__ENABLED -> nacos.enabled
        );

    let cfg = builder.build()?;
    cfg.try_deserialize::<Settings>()
}

#[derive(Debug, Deserialize)]
struct RoutesFile {
    routes: Vec<RouteRule>,
    #[serde(default)]
    health_check: Option<HealthCheckConfig>,
}

pub fn load_route_rules() -> Result<(Vec<RouteRule>, HealthCheckConfig), ConfigError> {
    // 可执行文件同级目录
    let exe_dir: PathBuf = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // 打包后同目录下的 routes.toml
    let packaged_routes = exe_dir.join("routes.toml");

    // 构建配置，优先读取项目根目录的 routes.toml，其次读取打包目录
    let c = Config::builder()
        .add_source(File::with_name("routes").required(false)) // 开发时：./routes.toml
        .add_source(File::from(packaged_routes).required(false)) // 部署时：bin 同目录
        .build()?;

    // 反序列化到结构体
    let mut rf: RoutesFile = c.try_deserialize()?;

    // 校验并预编译所有路由规则
    for (i, rule) in rf.routes.iter_mut().enumerate() {
        if let Err(err) = rule.validate() {
            return Err(ConfigError::Message(format!(
                "路由规则 #{} 配置错误: {}", i + 1, err
            )));
        }
        // 启动时预编译正则，避免运行时 Mutex 竞争
        rule.compile_patterns();
    }

    let health_config = rf.health_check.unwrap_or_default();
    Ok((rf.routes, health_config))
}

/// 创建共享路由规则（启动时调用）
pub fn create_shared_route_rules(rules: Vec<RouteRule>) -> SharedRouteRules {
    Arc::new(ArcSwap::from_pointee(rules))
}

/// 热重载路由规则：重新读取 routes.toml 并原子替换
/// 当 Nacos 路由激活时，本地文件重载将被跳过
pub fn reload_route_rules(shared: &SharedRouteRules) -> Result<usize, String> {
    if is_nacos_routes_active() {
        tracing::info!("⏭ 跳过本地路由重载：Nacos 路由已激活，优先级更高");
        return Err("Nacos 路由已激活，本地 routes.toml 变更已忽略".to_string());
    }
    match load_route_rules() {
        Ok((new_rules, _health_config)) => {
            let count = new_rules.len();
            shared.store(Arc::new(new_rules));
            tracing::info!("🔄 路由热重载成功，加载 {} 条规则", count);
            Ok(count)
        }
        Err(e) => {
            tracing::error!("路由热重载失败: {}", e);
            Err(format!("路由重载失败: {}", e))
        }
    }
}

/// 启动 routes.toml 文件监听器，检测到变更时自动重载
pub fn start_route_watcher(shared: SharedRouteRules) {
    use notify::{Watcher, RecursiveMode, Event, EventKind, event::ModifyKind};
    use std::sync::mpsc;

    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("无法创建文件监听器: {}", e);
                return;
            }
        };

        // 监听当前目录下的 routes.toml
        let watch_path = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if let Err(e) = watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
            tracing::error!("无法监听目录 {:?}: {}", watch_path, e);
            return;
        }

        tracing::info!("📂 路由文件监听器已启动，监听: {:?}/routes.toml", watch_path);

        // 防抖：记录上次重载时间
        let mut last_reload = std::time::Instant::now();
        let debounce_duration = Duration::from_secs(2);

        for event in rx {
            match event {
                Ok(Event { kind: EventKind::Modify(ModifyKind::Data(_)), paths, .. })
                | Ok(Event { kind: EventKind::Modify(ModifyKind::Any), paths, .. }) => {
                    let is_routes = paths.iter().any(|p| {
                        p.file_name()
                            .map(|f| f.to_string_lossy().contains("routes"))
                            .unwrap_or(false)
                    });
                    if is_routes && last_reload.elapsed() > debounce_duration {
                        tracing::info!("检测到 routes.toml 变更，触发热重载...");
                        let _ = reload_route_rules(&shared);
                        last_reload = std::time::Instant::now();
                    }
                }
                Err(e) => tracing::warn!("文件监听错误: {}", e),
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_rule_matching() {
        let mut routes = vec![
            RouteRule {
                prefix: vec!["/user".to_string(), "/users".to_string()],
                upstream: vec!["http://localhost:30000".to_string()],
                strategy: "robin".to_string(),
                whitelist: None,
                compiled_prefixes: vec![],
                compiled_whitelist: vec![],
                service_name: None,
            },
            RouteRule {
                prefix: vec!["/api/user/{id}".to_string()],
                upstream: vec!["http://localhost:30001".to_string(), "http://localhost:30002".to_string()],
                strategy: "random".to_string(),
                whitelist: None,
                compiled_prefixes: vec![],
                compiled_whitelist: vec![],
                service_name: None,
            },
        ];

        // 预编译模式
        for rule in &mut routes {
            rule.compile_patterns();
        }

        let test_cases = vec![
            ("/user", true, "30000"),
            ("/users", true, "30000"),
            ("/api/user/123", true, "30001或30002"),
        ];

        for (path, _should_match, expected_upstream) in test_cases {
            let mut matched = false;
            for route in &routes {
                if route.matches(path) {
                    if route.upstream.len() == 1 {
                        assert_eq!(route.upstream[0], format!("http://localhost:{}", expected_upstream));
                    }
                    matched = true;
                    break;
                }
            }
            assert!(matched, "路径 {} 应该匹配某个路由", path);
        }
    }

    #[test]
    fn test_route_rule_validation() {
        let valid_route = RouteRule {
            prefix: vec!["/user".to_string()],
            upstream: vec!["http://localhost:30000".to_string()],
            strategy: "robin".to_string(),
            whitelist: None,
            compiled_prefixes: vec![],
            compiled_whitelist: vec![],
            service_name: None,
        };
        assert!(valid_route.validate().is_ok());

        let invalid_prefix = RouteRule {
            prefix: vec![],
            upstream: vec!["http://localhost:30000".to_string()],
            strategy: "robin".to_string(),
            whitelist: None,
            compiled_prefixes: vec![],
            compiled_whitelist: vec![],
            service_name: None,
        };
        assert!(invalid_prefix.validate().is_err());

        let invalid_upstream = RouteRule {
            prefix: vec!["/user".to_string()],
            upstream: vec![],
            strategy: "robin".to_string(),
            whitelist: None,
            compiled_prefixes: vec![],
            compiled_whitelist: vec![],
            service_name: None,
        };
        assert!(invalid_upstream.validate().is_err());

        let invalid_strategy = RouteRule {
            prefix: vec!["/user".to_string()],
            upstream: vec!["http://localhost:30000".to_string()],
            strategy: "unknown".to_string(),
            whitelist: None,
            compiled_prefixes: vec![],
            compiled_whitelist: vec![],
            service_name: None,
        };
        assert!(invalid_strategy.validate().is_err());
    }
}
