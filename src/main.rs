// mini-proxy 入口：加载配置 → 初始化日志 → 启动服务
// 首次运行若无 config.toml，自动生成并退出，提示用户填写后重启

mod config;
mod log;
mod protocol;
mod retry;
mod server;
mod upstream;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

// 示例配置嵌入二进制，首次运行时写入磁盘
const EXAMPLE_CONFIG: &str = include_str!("../config.example.toml");

#[tokio::main]
async fn main() -> Result<()> {
    // 处理 -h / --help：输出配置模板
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("mini-proxy - 简洁版 AI API 代理（同渠道自动重试）\n");
        println!("用法:");
        println!("  mini-proxy              运行服务（默认读 config.toml）");
        println!("  mini-proxy -h|--help    显示此帮助（含配置模板）");
        println!("  MINI_PROXY_CONFIG=xxx.toml mini-proxy   指定配置文件\n");
        println!("首次运行若未发现 config.toml，会自动生成示例配置并退出。\n");
        println!("对外服务端点:");
        println!("  POST /chat/completions  → OpenAI 协议");
        println!("  POST /messages          → Claude 协议\n");
        println!("===== 配置模板（config.toml）=====");
        print!("{}", EXAMPLE_CONFIG);
        return Ok(());
    }

    let config_path = std::env::var("MINI_PROXY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config.toml"));

    // 首次运行：config.toml 不存在 → 生成并退出
    if !config_path.exists() {
        println!("未发现配置文件：{}", config_path.display());
        println!("已自动生成示例配置，请填写真实配置后重新启动。");
        std::fs::write(&config_path, EXAMPLE_CONFIG)?;
        // 退出，不启动服务
        return Ok(());
    }

    let cfg = config::Config::load(&config_path)?;
    log::init(&cfg.log)?;
    tracing::info!(config_path = %config_path.display(), "配置加载完成");

    // 启动时打印渠道信息
    for p in &cfg.provider {
        if let Some(ep) = p.openai_endpoint() {
            tracing::info!(
                provider = %p.name,
                protocol = "openai",
                base_url = %ep.base_url,
                models = ?ep.models,
                max_retries = ep.max_retries,
                key_mode = ?ep.key_mode,
                "已加载渠道"
            );
        }
        if let Some(ep) = p.claude_endpoint() {
            tracing::info!(
                provider = %p.name,
                protocol = "claude",
                base_url = %ep.base_url,
                models = ?ep.models,
                max_retries = ep.max_retries,
                key_mode = ?ep.key_mode,
                "已加载渠道"
            );
        }
    }

    let client = Arc::new(upstream::UpstreamClient::new());
    let state = server::AppState {
        config: Arc::new(cfg.clone()),
        client,
    };
    let app = server::build(state);

    let listener = tokio::net::TcpListener::bind(&cfg.server.listen).await?;
    tracing::info!(listen = %cfg.server.listen, "服务启动完成，监听本地端口");
    axum::serve(listener, app).await?;
    Ok(())
}
