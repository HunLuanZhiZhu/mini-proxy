// 协议适配：OpenAI、Anthropic、Response 三种，各自透传不做转换
// 路由硬编码：/v1/* → OpenAI，/v2/* → Anthropic，/v3/* → Response

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAI,
    Anthropic,
    Responses,
}

impl Protocol {
    // 按路径前缀判定协议：/v1 → OpenAI，/v2 → Anthropic，/v3 → Responses
    pub fn from_path(path: &str) -> Option<Self> {
        let p = path.trim_start_matches('/');
        if p.starts_with("v1") {
            Some(Protocol::OpenAI)
        } else if p.starts_with("v2") {
            Some(Protocol::Anthropic)
        } else if p.starts_with("v3") {
            Some(Protocol::Responses)
        } else {
            None
        }
    }

    // path_mode = append 时的追加后缀
    pub fn append_suffix(&self) -> &'static str {
        match self {
            Protocol::OpenAI => "/chat/completions",
            Protocol::Anthropic => "/v1/messages",
            Protocol::Responses => "/responses",
        }
    }
}
