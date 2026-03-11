use crate::config::NacosSettings;
use nacos_rust_client::client::AuthInfo;
use once_cell::sync::OnceCell;
use std::sync::Arc;

/// Nacos 命名客户端（服务注册/发现）
static NAMING_CLIENT: OnceCell<Arc<nacos_rust_client::client::naming_client::NamingClient>> =
    OnceCell::new();

/// Nacos 配置客户端（配置管理）
static CONFIG_CLIENT: OnceCell<Arc<nacos_rust_client::client::config_client::ConfigClient>> =
    OnceCell::new();

/// 构建认证信息
fn build_auth(config: &NacosSettings) -> Option<AuthInfo> {
    match (&config.username, &config.password) {
        (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => {
            Some(AuthInfo::new(u, p))
        }
        _ => None,
    }
}

/// 初始化 Nacos 客户端
/// 配置中心和服务注册/发现使用独立的命名空间和组，互不干扰
pub async fn init_nacos(config: &NacosSettings) -> Result<(), anyhow::Error> {
    use nacos_rust_client::client::ClientBuilder;

    let server_addr = &config.server_addrs;
    let auth_info = build_auth(config);
    let app_name = config.service_name.clone().unwrap_or_else(|| "ms-gateway".to_string());

    // 配置中心和服务注册的命名空间是否相同
    let same_namespace = config.config_namespace == config.naming_namespace;

    if same_namespace {
        // 命名空间相同：共享一个 ClientBuilder
        let builder = ClientBuilder::new()
            .set_endpoint_addrs(server_addr)
            .set_auth_info(auth_info.clone())
            .set_use_grpc(true)
            .set_tenant(config.config_namespace.clone())
            .set_app_name(app_name);

        let (config_client, naming_client) = builder.build();

        CONFIG_CLIENT.set(config_client)
            .map_err(|_| anyhow::anyhow!("Nacos ConfigClient 已初始化"))?;
        NAMING_CLIENT.set(naming_client)
            .map_err(|_| anyhow::anyhow!("Nacos NamingClient 已初始化"))?;

        tracing::info!(
            "✅ Nacos 客户端初始化成功 (共享命名空间: {}, 服务器: {}, 认证: {})",
            config.config_namespace, server_addr,
            if auth_info.is_some() { "已启用" } else { "未启用" }
        );
    } else {
        // 命名空间不同：分别创建两个 ClientBuilder
        // 1) 配置中心客户端
        let config_builder = ClientBuilder::new()
            .set_endpoint_addrs(server_addr)
            .set_auth_info(auth_info.clone())
            .set_use_grpc(true)
            .set_tenant(config.config_namespace.clone())
            .set_app_name(format!("{}-config", app_name));
        let (config_client, _) = config_builder.build();
        CONFIG_CLIENT.set(config_client)
            .map_err(|_| anyhow::anyhow!("Nacos ConfigClient 已初始化"))?;

        // 2) 服务注册/发现客户端
        let naming_builder = ClientBuilder::new()
            .set_endpoint_addrs(server_addr)
            .set_auth_info(auth_info.clone())
            .set_use_grpc(true)
            .set_tenant(config.naming_namespace.clone())
            .set_app_name(format!("{}-naming", app_name));
        let (_, naming_client) = naming_builder.build();
        NAMING_CLIENT.set(naming_client)
            .map_err(|_| anyhow::anyhow!("Nacos NamingClient 已初始化"))?;

        tracing::info!(
            "✅ Nacos 客户端初始化成功 (配置命名空间: {}, 服务命名空间: {}, 服务器: {}, 认证: {})",
            config.config_namespace, config.naming_namespace, server_addr,
            if auth_info.is_some() { "已启用" } else { "未启用" }
        );
    }

    Ok(())
}

/// 获取 NamingClient
pub fn get_naming_client() -> Option<Arc<nacos_rust_client::client::naming_client::NamingClient>> {
    NAMING_CLIENT.get().cloned()
}

/// 获取 ConfigClient
pub fn get_config_client() -> Option<Arc<nacos_rust_client::client::config_client::ConfigClient>> {
    CONFIG_CLIENT.get().cloned()
}
