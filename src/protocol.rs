// 协议适配：OpenAI 与 Claude 两种，各自透传不做转换
// 路由硬编码：/v1/* → OpenAI，/v2/* → Claude

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAI,
    Claude,
}

impl Protocol {
    // 按路径前缀判定协议：/v1 → OpenAI，/v2 → Claude
    pub fn from_path(path: &str) -> Option<Self> {
        let p = path.trim_start_matches('/');
        if p.starts_with("v1") {
            Some(Protocol::OpenAI)
        } else if p.starts_with("v2") {
            Some(Protocol::Claude)
        } else {
            None
        }
    }

    // path_mode = append 时的追加后缀
    pub fn append_suffix(&self) -> &'static str {
        match self {
            Protocol::OpenAI => "/chat/completions",
            Protocol::Claude => "/messages",
        }
    }
}
