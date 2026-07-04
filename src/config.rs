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
    // 业务错误码重试：解析响应 body 中的 error.code 字段，命中则重试
    #[serde(default)]
    pub retry_on_code: Option<Vec<i64>>,
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
    pub anthropic: Option<EndpointRaw>,
    #[serde(default)]
    pub responses: Option<EndpointRaw>,
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
    pub retry_on_code: Option<Vec<i64>>,
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
    pub retry_on_code: Vec<i64>,
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
    fn default() -> Self { KeyMode::Passthrough }
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

// 支持单值 429 或范围字符串 "500-504"
// 用 serde_json::Value 接收再手动解析，避开 flatten + untagged 空数组的类型推断 bug
#[derive(Debug, Clone, Deserialize)]
pub struct StatusSpec(pub serde_json::Value);

impl StatusSpec {
    pub fn to_range(&self) -> Option<std::ops::RangeInclusive<u16>> {
        match &self.0 {
            serde_json::Value::Number(n) => n.as_u64().map(|c| c as u16..=c as u16),
            serde_json::Value::String(s) => {
                if let Some((a, b)) = s.split_once('-') {
                    let a = a.trim().parse::<u16>().ok()?;
                    let b = b.trim().parse::<u16>().ok()?;
                    Some(a..=b)
                } else {
                    s.trim().parse::<u16>().ok().map(|c| c..=c)
                }
            }
            _ => None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct StatusMatcher {
    ranges: Vec<std::ops::RangeInclusive<u16>>,
}

impl StatusMatcher {
    pub fn from_specs(specs: &[StatusSpec]) -> Self {
        let ranges: Vec<_> = specs.iter().filter_map(|s| s.to_range()).collect();
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
        StatusSpec(serde_json::Value::String("100-199".into())),
        StatusSpec(serde_json::Value::String("300-399".into())),
        StatusSpec(serde_json::Value::String("401-407".into())),
        StatusSpec(serde_json::Value::String("409-499".into())),
        StatusSpec(serde_json::Value::String("500-503".into())),
        StatusSpec(serde_json::Value::String("505-523".into())),
        StatusSpec(serde_json::Value::String("525-599".into())),
    ]
}

pub fn is_always_skip(code: u16) -> bool {
    matches!(code, 504 | 524)
}

// 讯飞 Coding Plan 默认可重试业务错误码（响应 body 中的 error.code 字段）
// 这些是临时性错误，可能随额度刷新/引擎恢复而成功：
//   10007 流量受限、10008 服务容量不足、10009 引擎连接失败、10010 引擎排队、
//   10012 引擎内部错误/排队、10110 服务忙、10222 引擎网络异常、10223 LB找不到引擎、
//   11200 授权/业务量超限、11201 次数超限、11202 秒级流控、11203 并发流控、11210 tpm超限
pub fn default_retry_codes() -> Vec<i64> {
    vec![
        10007, 10008, 10009, 10010, 10012, 10110, 10222, 10223,
        11200, 11201, 11202, 11203, 11210,
    ]
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
            retry_on_code: raw
                .retry_on_code
                .clone()
                .or_else(|| c.retry_on_code.clone())
                .unwrap_or_default(),
            max_retries: raw.max_retries.or(c.max_retries).unwrap_or(10000),
            path_mode: raw.path_mode.or(c.path_mode).unwrap_or_default(),
        })
    }

    pub fn openai_endpoint(&self) -> Option<Endpoint> {
        self.openai.as_ref().and_then(|r| self.resolve_endpoint(r))
    }

    pub fn anthropic_endpoint(&self) -> Option<Endpoint> {
        self.anthropic.as_ref().and_then(|r| self.resolve_endpoint(r))
    }

    pub fn responses_endpoint(&self) -> Option<Endpoint> {
        self.responses.as_ref().and_then(|r| self.resolve_endpoint(r))
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

    // 业务错误码列表：空则用默认（讯飞可重试码）
    pub fn retry_codes(&self) -> Vec<i64> {
        if self.retry_on_code.is_empty() {
            default_retry_codes()
        } else {
            self.retry_on_code.clone()
        }
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
