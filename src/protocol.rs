// 协议适配：OpenAI 与 Claude 两种，各自透传不做转换

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAI,
    Claude,
}

impl Protocol {
    // 根据客户端访问路径后缀判定协议，不识别路径前缀（/v1、/api/xxx 等都接受）
    pub fn from_path(path: &str) -> Option<Self> {
        let p = path.trim_end_matches('/');
        if p.ends_with("/chat/completions") {
            Some(Protocol::OpenAI)
        } else if p.ends_with("/messages") {
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
