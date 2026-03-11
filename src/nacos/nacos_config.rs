use nacos_rust_client::client::config_client::{listener::ConfigListener, ConfigKey};

use crate::config::{NacosSettings, SharedRouteRules, set_nacos_routes_active};

/// 订阅路由配置变更（启动时 listener 会立即收到当前值，等同于 fetch + subscribe）
pub async fn subscribe_route_changes(config: &NacosSettings, shared_rules: &SharedRouteRules) {
    let (data_id, group) = match &config.routes_data_id {
        Some(did) => (
            did.clone(),
            config.routes_group.clone().unwrap_or_else(|| config.config_group.clone()),
        ),
        None => return,
    };

    let config_client = match super::client::get_config_client() {
        Some(c) => c,
        None => return,
    };

    let tenant = config.config_namespace.clone();
    let naming_group = config.naming_group.clone();
    let shared = shared_rules.clone();

    struct RouteConfigListener {
        data_id: String,
        group: String,
        tenant: String,
        naming_group: String,
        shared_rules: SharedRouteRules,
    }

    impl ConfigListener for RouteConfigListener {
        fn get_key(&self) -> ConfigKey {
            ConfigKey::new(&self.data_id, &self.group, &self.tenant)
        }

        fn change(&self, key: &ConfigKey, value: &str) {
            tracing::info!(
                "🔄 Nacos 路由配置变更: {} ({}字节)",
                key.data_id, value.len()
            );
            apply_routes_from_toml(value, &self.shared_rules, &self.naming_group);
        }
    }

    let listener = Box::new(RouteConfigListener {
        data_id: data_id.clone(),
        group,
        tenant,
        naming_group,
        shared_rules: shared,
    });

    match config_client.subscribe(listener).await {
        Ok(_) => tracing::info!("📡 已订阅 Nacos 路由配置变更: {}", data_id),
        Err(e) => tracing::error!("订阅 Nacos 路由配置失败: {}", e),
    }
}

/// 将 TOML 格式的路由配置解析并应用到 SharedRouteRules
/// 同时自动订阅路由规则中引用的新服务
fn apply_routes_from_toml(content: &str, shared_rules: &SharedRouteRules, naming_group: &str) {
    use std::sync::Arc;

    // 复用 config 模块的反序列化结构
    #[derive(serde::Deserialize)]
    struct RoutesContent {
        routes: Vec<crate::config::RouteRule>,
    }

    match toml::from_str::<RoutesContent>(content) {
        Ok(mut parsed) => {
            for (i, rule) in parsed.routes.iter_mut().enumerate() {
                if let Err(e) = rule.validate() {
                    tracing::error!("Nacos 路由规则 #{} 校验失败: {}", i + 1, e);
                    return;
                }
                rule.compile_patterns();
            }

            // 提取所有 service_name（在 store 之前，以便订阅）
            let service_names: Vec<String> = parsed.routes
                .iter()
                .filter_map(|r| r.service_name.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            let count = parsed.routes.len();
            shared_rules.store(Arc::new(parsed.routes));
            set_nacos_routes_active(true);
            tracing::info!("✅ Nacos 路由规则已应用: {} 条", count);

            // 自动订阅路由中引用的服务（在后台执行，不阻塞 listener）
            if !service_names.is_empty() {
                let grp = naming_group.to_string();
                tokio::spawn(async move {
                    for svc in &service_names {
                        super::discovery::subscribe_one_service(svc, &grp).await;
                    }
                });
            }
        }
        Err(e) => {
            tracing::error!("Nacos 路由配置解析失败: {}", e);
        }
    }
}
