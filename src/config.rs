// 配置结构与加载

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub log: LogConfig,
    #[serde(default)]
    pub provider: Vec<Provider>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub listen: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct LogConfig {
    #[serde(default = "default_level")]
    pub level: String,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_true")]
    pub to_stdout: bool,
    #[serde(default)]
    pub to_file: String,
    #[serde(default = "default_rotate_size")]
    pub rotate_size_mb: u64,
    #[serde(default = "default_rotate_keep")]
    pub rotate_keep: usize,
}

fn default_level() -> String { "info".into() }
fn default_format() -> String { "pretty".into() }
fn default_true() -> bool { true }
fn default_rotate_size() -> u64 { 50 }
fn default_rotate_keep() -> usize { 7 }

#[derive(Debug, Clone, Deserialize)]
pub struct Provider {
    pub name: String,
    #[serde(default)]
    pub openai: Option<Endpoint>,
    #[serde(default)]
    pub claude: Option<Endpoint>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub retry_on_status: Vec<StatusSpec>,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub path_mode: PathMode,
}

fn default_max_retries() -> u32 { 0 }

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PathMode {
    Append,
    Full,
}

impl Default for PathMode {
    fn default() -> Self { PathMode::Append }
}

// 支持单值 429 或范围字符串 "500-504"
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StatusSpec {
    Single(u16),
    Range(String),
}

// 状态码匹配器，预解析为范围集合
#[derive(Debug, Clone)]
pub struct StatusMatcher {
    ranges: Vec<std::ops::RangeInclusive<u16>>,
}

impl StatusMatcher {
    pub fn from_specs(specs: &[StatusSpec]) -> Self {
        let mut ranges = Vec::new();
        for s in specs {
            match s {
                StatusSpec::Single(c) => ranges.push(*c..=*c),
                StatusSpec::Range(r) => {
                    if let Some((a, b)) = r.split_once('-') {
                        if let (Ok(a), Ok(b)) = (a.parse::<u16>(), b.parse::<u16>()) {
                            ranges.push(a..=b);
                        }
                    } else if let Ok(c) = r.parse::<u16>() {
                        ranges.push(c..=c);
                    }
                }
            }
        }
        StatusMatcher { ranges }
    }

    pub fn matches(&self, code: u16) -> bool {
        self.ranges.iter().any(|r| r.contains(&code))
    }
}

impl Endpoint {
    pub fn status_matcher(&self) -> StatusMatcher {
        StatusMatcher::from_specs(&self.retry_on_status)
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        toml::from_str(&content).with_context(|| "解析配置文件失败".to_string())
    }
}
