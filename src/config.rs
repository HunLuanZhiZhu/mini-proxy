// 配置结构与加载

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
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
    // Key 模式：
    //   override（默认）：用 config 中的 api_key 覆盖客户端发来的任意 Key
    //   passthrough：不存储不管理，保留客户端请求中的原 Key 不变
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_key_mode")]
    pub key_mode: KeyMode,
    #[serde(default)]
    pub models: Vec<String>,
    // 模型 ID 映射：客户端模型名 → 上游模型名，未配置则同名透传
    #[serde(default)]
    pub model_map: HashMap<String, String>,
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
pub enum KeyMode {
    // 用 config 的 api_key 覆盖客户端 Key
    Override,
    // 保留客户端请求中的原 Key，config 不存储不管理
    Passthrough,
}

impl Default for KeyMode {
    fn default() -> Self { KeyMode::Override }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PathMode {
    Append,
    Full,
}

impl Default for PathMode {
    fn default() -> Self { PathMode::Append }
}

fn default_key_mode() -> KeyMode { KeyMode::Override }

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

// 参考 new-api 默认重试状态码范围：
// 100-199, 300-399, 401-407, 409-499, 500-503, 505-523, 525-599
// 永远跳过 504 和 524（alwaysSkipRetryStatusCodes）
pub fn default_retry_specs() -> Vec<StatusSpec> {
    vec![
        StatusSpec::Range("100-199".into()),
        StatusSpec::Range("300-399".into()),
        StatusSpec::Range("401-407".into()),
        StatusSpec::Range("409-499".into()),
        StatusSpec::Range("500-503".into()),
        StatusSpec::Range("505-523".into()),
        StatusSpec::Range("525-599".into()),
    ]
}

// 永远不重试的状态码（即使落在 retry_on_status 范围内）
pub fn is_always_skip(code: u16) -> bool {
    matches!(code, 504 | 524)
}

impl Endpoint {
    pub fn status_matcher(&self) -> StatusMatcher {
        let specs = if self.retry_on_status.is_empty() {
            default_retry_specs()
        } else {
            self.retry_on_status.clone()
        };
        StatusMatcher::from_specs(&specs)
    }

    // 模型名映射：未配置则原样返回
    pub fn map_model(&self, client_model: &str) -> String {
        self.model_map
            .get(client_model)
            .cloned()
            .unwrap_or_else(|| client_model.to_string())
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        toml::from_str(&content).with_context(|| "解析配置文件失败".to_string())
    }
}
