use config::{Config, ConfigError, File};
use serde::Deserialize;
use std::{env, path::PathBuf, time::Duration};
use crate::path_matcher::RoutePattern;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone)]
pub struct RouteRule {
    // 支持单个或多个前缀
    #[serde(with = "prefix_deserializer")]
    pub prefix: Vec<String>,
    // 支持单个或多个上游
    #[serde(with = "upstream_deserializer")]
    pub upstream: Vec<String>,
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

// 自定义反序列化器，支持字符串和数组两种格式
mod prefix_deserializer {
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

mod upstream_deserializer {
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
}

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
        if self.upstream.is_empty() {
            return Err("upstream不能为空".to_string());
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
        .add_source(config::Environment::default());

    let cfg = builder.build()?;
    cfg.try_deserialize::<Settings>()
}

#[derive(Debug, Deserialize)]
struct RoutesFile { routes: Vec<RouteRule> }

pub fn load_route_rules() -> Result<Vec<RouteRule>, ConfigError> {
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

    Ok(rf.routes)
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
            },
            RouteRule {
                prefix: vec!["/api/user/{id}".to_string()],
                upstream: vec!["http://localhost:30001".to_string(), "http://localhost:30002".to_string()],
                strategy: "random".to_string(),
                whitelist: None,
                compiled_prefixes: vec![],
                compiled_whitelist: vec![],
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
        };
        assert!(valid_route.validate().is_ok());

        let invalid_prefix = RouteRule {
            prefix: vec![],
            upstream: vec!["http://localhost:30000".to_string()],
            strategy: "robin".to_string(),
            whitelist: None,
            compiled_prefixes: vec![],
            compiled_whitelist: vec![],
        };
        assert!(invalid_prefix.validate().is_err());

        let invalid_upstream = RouteRule {
            prefix: vec!["/user".to_string()],
            upstream: vec![],
            strategy: "robin".to_string(),
            whitelist: None,
            compiled_prefixes: vec![],
            compiled_whitelist: vec![],
        };
        assert!(invalid_upstream.validate().is_err());

        let invalid_strategy = RouteRule {
            prefix: vec!["/user".to_string()],
            upstream: vec!["http://localhost:30000".to_string()],
            strategy: "unknown".to_string(),
            whitelist: None,
            compiled_prefixes: vec![],
            compiled_whitelist: vec![],
        };
        assert!(invalid_strategy.validate().is_err());
    }
}
