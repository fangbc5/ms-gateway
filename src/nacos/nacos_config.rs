use nacos_rust_client::client::config_client::{listener::ConfigListener, ConfigKey};

use crate::config::{NacosSettings, SharedRouteRules};

/// 从 Nacos 拉取路由规则并应用（启动时调用一次）
pub async fn fetch_and_apply_routes(config: &NacosSettings, shared_rules: &SharedRouteRules) {
    let (data_id, group) = match &config.routes_data_id {
        Some(did) => (
            did.clone(),
            config.routes_group.clone().unwrap_or_else(|| config.group.clone()),
        ),
        None => return, // 未配置 routes_data_id，跳过
    };

    let config_client = match super::client::get_config_client() {
        Some(c) => c,
        None => return,
    };

    let tenant = config.namespace.clone().unwrap_or_default();
    let key = ConfigKey::new(&data_id, &group, &tenant);

    match config_client.get_config(&key).await {
        Ok(content) => {
            tracing::info!(
                "📥 从 Nacos 获取路由配置成功: {} ({}字节)",
                data_id, content.len()
            );
            apply_routes_from_toml(&content, shared_rules);
        }
        Err(e) => {
            tracing::warn!("从 Nacos 获取路由配置失败: {}，使用本地配置", e);
        }
    }
}

/// 订阅路由配置变更（热更新）
pub async fn subscribe_route_changes(config: &NacosSettings, shared_rules: &SharedRouteRules) {
    let (data_id, group) = match &config.routes_data_id {
        Some(did) => (
            did.clone(),
            config.routes_group.clone().unwrap_or_else(|| config.group.clone()),
        ),
        None => return,
    };

    let config_client = match super::client::get_config_client() {
        Some(c) => c,
        None => return,
    };

    let tenant = config.namespace.clone().unwrap_or_default();
    let shared = shared_rules.clone();

    struct RouteConfigListener {
        data_id: String,
        group: String,
        tenant: String,
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
            apply_routes_from_toml(value, &self.shared_rules);
        }
    }

    let listener = Box::new(RouteConfigListener {
        data_id: data_id.clone(),
        group,
        tenant,
        shared_rules: shared,
    });

    match config_client.subscribe(listener).await {
        Ok(_) => tracing::info!("📡 已订阅 Nacos 路由配置变更: {}", data_id),
        Err(e) => tracing::error!("订阅 Nacos 路由配置失败: {}", e),
    }
}

/// 将 TOML 格式的路由配置解析并应用到 SharedRouteRules
fn apply_routes_from_toml(content: &str, shared_rules: &SharedRouteRules) {
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
            let count = parsed.routes.len();
            shared_rules.store(Arc::new(parsed.routes));
            tracing::info!("✅ Nacos 路由规则已应用: {} 条", count);
        }
        Err(e) => {
            tracing::error!("Nacos 路由配置解析失败: {}", e);
        }
    }
}
