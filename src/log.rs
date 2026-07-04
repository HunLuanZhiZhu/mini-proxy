// 日志系统：stdout + 文件双输出，按天滚动

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, prelude::*, util::SubscriberInitExt, EnvFilter};

use crate::config::LogConfig;

pub fn init(cfg: &LogConfig) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cfg.level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let mut layers: Vec<Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync + 'static>> =
        Vec::new();

    if cfg.to_stdout {
        let l: Box<dyn tracing_subscriber::Layer<_> + Send + Sync + 'static> = match cfg.format.as_str() {
            "json" => tracing_subscriber::fmt::layer()
                .json()
                .with_filter(filter.clone())
                .boxed(),
            _ => tracing_subscriber::fmt::layer()
                .pretty()
                .with_filter(filter.clone())
                .boxed(),
        };
        layers.push(l);
    }

    if !cfg.to_file.is_empty() {
        let parent = std::path::Path::new(&cfg.to_file)
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent).ok();
        let prefix = std::path::Path::new(&cfg.to_file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("proxy");
        let suffix = std::path::Path::new(&cfg.to_file)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("log");
        let appender = tracing_appender::rolling::daily(parent, format!("{}.{}", prefix, suffix));
        layers.push(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(appender)
                .with_filter(filter.clone())
                .boxed(),
        );
    }

    let subscriber = tracing_subscriber::registry().with(layers);
    subscriber.init();
    Ok(())
}
