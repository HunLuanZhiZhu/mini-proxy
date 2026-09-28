// 协议适配：OpenAI、Anthropic、Response 三种，各自透传不做转换
// 路由按裸路径判定（去掉 /v1 /v2 /v3 前缀，避免与协议自身路径混淆）：
//   /chat/completions → OpenAI，/v1/messages → Anthropic，/responses → Responses

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAI,
    Anthropic,
    Responses,
}

impl Protocol {
    // 按完整裸路径判定协议
    pub fn from_path(path: &str) -> Option<Self> {
        let p = path.trim_start_matches('/');
        if p == "chat/completions" || p.ends_with("/chat/completions") {
            Some(Protocol::OpenAI)
        } else if p == "v1/messages" || p.ends_with("/v1/messages") {
            Some(Protocol::Anthropic)
        } else if p == "responses" || p.ends_with("/responses") {
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

    // 默认思考强度最高档：
    //   OpenAI / Responses → "xhigh"（OpenAI 官方 spec 最高档，无 max）
    //   Anthropic          → "max"（output_config.effort 官方枚举最高档）
    pub fn default_effort(&self) -> &'static str {
        match self {
            Protocol::OpenAI | Protocol::Responses => "xhigh",
            Protocol::Anthropic => "max",
        }
    }
}
