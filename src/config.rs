// 配置结构与加载
// Provider 级字段（api_key/models/model_map/max_retries 等）可被两种协议共用
// Endpoint 级字段覆盖 Provider 级同名字段

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

// Provider 级共用字段（可选），Endpoint 级同名字段覆盖之
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderCommon {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub model_map: Option<HashMap<String, String>>,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub retry_on_status: Option<Vec<StatusSpec>>,
    #[serde(default)]
    pub key_mode: Option<KeyMode>,
    #[serde(default)]
    pub path_mode: Option<PathMode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Provider {
    pub name: String,
    #[serde(flatten)]
    pub common: ProviderCommon,
    #[serde(default)]
    pub openai: Option<EndpointRaw>,
    #[serde(default)]
    pub claude: Option<EndpointRaw>,
}

// 原始 endpoint 配置，字段全可选，缺失的从 provider 级取
#[derive(Debug, Clone, Deserialize, Default)]
pub struct EndpointRaw {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub models: Option<Vec<String>>,
    pub model_map: Option<HashMap<String, String>>,
    pub max_retries: Option<u32>,
    pub retry_on_status: Option<Vec<StatusSpec>>,
    pub key_mode: Option<KeyMode>,
    pub path_mode: Option<PathMode>,
}

// 合并后的有效 endpoint
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: String,
    pub key_mode: KeyMode,
    pub models: Vec<String>,
    pub model_map: HashMap<String, String>,
    pub retry_on_status: Vec<StatusSpec>,
    pub max_retries: u32,
    pub path_mode: PathMode,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeyMode {
    Override,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StatusSpec {
    Single(u16),
    Range(String),
}

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

pub fn is_always_skip(code: u16) -> bool {
    matches!(code, 504 | 524)
}

impl Provider {
    // 合并 provider 级与 endpoint 级配置，endpoint 级优先
    pub fn resolve_endpoint(&self, raw: &EndpointRaw) -> Option<Endpoint> {
        let base_url = raw.base_url.clone().or_else(|| None)?;
        let c = &self.common;
        Some(Endpoint {
            base_url,
            api_key: raw.api_key.clone().or_else(|| c.api_key.clone()).unwrap_or_default(),
            key_mode: raw.key_mode.or(c.key_mode).unwrap_or_default(),
            models: raw.models.clone().or_else(|| c.models.clone()).unwrap_or_default(),
            model_map: raw.model_map.clone().or_else(|| c.model_map.clone()).unwrap_or_default(),
            retry_on_status: raw
                .retry_on_status
                .clone()
                .or_else(|| c.retry_on_status.clone())
                .unwrap_or_default(),
            max_retries: raw.max_retries.or(c.max_retries).unwrap_or(100),
            path_mode: raw.path_mode.or(c.path_mode).unwrap_or_default(),
        })
    }

    pub fn openai_endpoint(&self) -> Option<Endpoint> {
        self.openai.as_ref().and_then(|r| self.resolve_endpoint(r))
    }

    pub fn claude_endpoint(&self) -> Option<Endpoint> {
        self.claude.as_ref().and_then(|r| self.resolve_endpoint(r))
    }
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
