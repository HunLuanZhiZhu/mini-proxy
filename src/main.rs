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

// 示例配置硬编码在代码里（唯一事实来源），首次运行写入磁盘；
// `--example` 可把它导出为 config.example.toml（该文件是生成物，不要手工维护）
const EXAMPLE_CONFIG: &str = r#"# mini-proxy 配置文件
# 首次运行自动生成，默认配置即可直接使用，无需填写 API Key
# 本文件由 `mini-proxy --example` 导出，手工修改会被覆盖

# ════════════════════════════════════════════════════════════
# 一、字段说明（修改时参考此部分）
# ════════════════════════════════════════════════════════════
#
# [server]
#   listen              本地监听地址，客户端访问 http://<listen>
#   clean_empty_content 清洗请求体：补全 input 项缺失的 type:"message"（Response 协议），移除 content 为空/空白的项（默认 true）
#
# [log]
#   level           日志级别：trace | debug | info | warn | error
#   format          输出格式：pretty（彩色） | json
#   to_stdout       是否输出到控制台
#   to_file         文件输出路径，留空则不写文件
#   rotate_size_mb  单文件大小上限（MB），超过则滚动
#   rotate_keep     保留历史文件数量
#
# [[provider]]
#   name            供应商名称（仅用于日志标识）
#   api_key         API Key（仅 key_mode = "override" 时使用，为空则透传客户端 Key）
#   models          支持的模型列表（客户端请求的 model 字段需在此列表中）
#   model_map       模型 ID 映射：客户端模型名 → 上游模型名（通常不需要，默认同名透传）
#   max_retries     同渠道同模型最大重试次数（总尝试 = max_retries + 1），默认 10000
#   retry_on_status 触发重试的 HTTP 状态码，支持单值 429 或范围字符串 "500-504"
#                   留空则使用默认范围（参考 new-api：100-199,300-399,401-407,409-499,500-503,505-523,525-599）
#                   永远不重试 504 和 524
#   retry_on_code   触发重试的业务错误码（响应 body 中 error.code 字段），整数数组
#                   留空则使用讯飞默认码：10007,10008,10009,10010,10012,10110,10222,10223,11200,11201,11202,11203,11210,10310
#                   （流量受限/服务容量不足/引擎连接失败/排队/内部错误/服务忙/网络异常/LB找不到引擎/授权超限/次数超限/秒级流控/并发流控/tpm超限）
#   key_mode        API Key 模式：
#                     "passthrough"（默认）：保留客户端原 Key，config 不存储不管理
#                     "override"：用 config 的 api_key 覆盖客户端 Key（api_key 为空时回退到 passthrough）
#   path_mode       上游 URL 拼接模式：
#                     "append"（默认）：上游 URL = base_url + 协议后缀（见下方）
#                     "full"：base_url 原样使用，不补路径
#   thinking_effort 强制思考强度档位（按次计费时每次拉满）：
#                     具体档位（如 "xhigh"/"max"/"high"）→ 强制覆盖客户端该字段
#                     "passthrough" → 原样透传不修改
#                     缺省 → 默认各协议最高档：OpenAI/Responses=xhigh，Anthropic=max
#   headers         静态注入头（表）：客户端没带的键才补上，已有的不覆盖（主路径 + aux 均生效）
#                     x-opencode-session 无需配置：代理自动从请求体推导会话 ID
#                     （Anthropic 取 metadata.user_id 的 _session_ 后缀，OpenAI/Responses 取 user 字段），
#                     推导不出时用启动时随机生成的进程级 UUID；在此配置可固定该兜底值
#
# [provider.openai]       OpenAI 协议端点，后缀 /chat/completions
# [provider.anthropic]    Anthropic 协议端点，后缀 /v1/messages
# [provider.responses]    Response 协议端点，后缀 /responses
#   以上三个端点的字段继承 provider 级，同名时覆盖之
#
# [provider.aux]          辅助端点：未识别路径原样透传（如 GET /v1/models、POST /v1/messages/count_tokens）
#                         base_url 必填；目标 = base_url + 原路径（含查询串）
#                         不改 body、不做模型映射、不注入思考强度、不重试；path_mode 无效
#                         多个 provider 都配了 aux 时只用第一个（日志会提示）
#
# 对外服务端点（裸路径，无 /v1 /v2 /v3 前缀，按路径自动区分协议）：
#   完整路径：
#     POST http://127.0.0.1:7946/chat/completions  → OpenAI 协议
#     POST http://127.0.0.1:7946/v1/messages       → Anthropic 协议
#     POST http://127.0.0.1:7946/responses         → Response 协议
#   通常填写（SDK base_url）：
#     http://127.0.0.1:7946   → 三种协议共用，按路径自动区分

# ════════════════════════════════════════════════════════════
# 二、填写示例（完整字段展示，按需取消注释并修改）
# ════════════════════════════════════════════════════════════
#
# [[provider]]
# name = "AstronCodingPlan"
# api_key = "sk-your-real-key"           # 仅 override 模式需要，passthrough 可留空
# models = ["xopglm52", "xopglm51", "xopdeepseekv4pro", "xopkimik26", "auto", "xopdeepseekv4flash"]
# max_retries = 10000
# retry_on_status = ["100-199", "300-399", "401-407", "409-499", "500-503", "505-523", "525-599"]
# retry_on_code = [10007, 10008, 10009, 10010, 10012, 10110, 10222, 10223, 11200, 11201, 11202, 11203, 11210, 10310]
# key_mode = "passthrough"               # override | passthrough（默认）
# path_mode = "append"                   # append（默认） | full
# # 模型 ID 映射（通常不需要，默认同名透传）
# ## [provider.model_map]
# ## "client-gpt4" = "xopglm52"
# ## "client-claude" = "xopglm51"
#
# [provider.openai]
# base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/v2"
# thinking_effort = "xhigh"            # OpenAI 默认最高档；填 passthrough 透传
# # 如需覆盖 provider 级字段，在此填写，例如：
# # api_key = "openai-专用-key"
# # max_retries = 50
#
# [provider.anthropic]
# base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/anthropic"
# thinking_effort = "max"              # Anthropic 默认最高档；填 passthrough 透传
# # 同样可在此覆盖 provider 级任意字段
#
# [provider.responses]
# # Response 协议（OpenAI /v1/responses 端点）
# # append 模式补 /responses；讯飞此地址已含完整路径，用 path_mode = "full"
# thinking_effort = "xhigh"            # Responses 默认最高档；填 passthrough 透传
# path_mode = "full"
# base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/v1/responses"
#
# [provider.aux]
# # 辅助端点：未识别路径透传到 base_url + 原路径，例如客户端点「获取模型」时发的 GET /v1/models
# base_url = "https://api.example.com"
#
# [provider.headers]
# # 静态注入头（可选）：客户端没带的键才补上
# # x-opencode-session 默认自动生成（启动时随机 UUID），在此填写可固定兜底值
# # x-opencode-session = "6f1c2a34-90ab-4c5d-8e21-7b0a9c3d4e5f"

# ════════════════════════════════════════════════════════════
# 三、可运行配置（默认即可使用，按需修改）
# ════════════════════════════════════════════════════════════

[server]
listen = "127.0.0.1:7946"
clean_empty_content = true

[log]
level = "info"
format = "pretty"
to_stdout = true
to_file = "logs/proxy.log"
rotate_size_mb = 50
rotate_keep = 7

[[provider]]
name = "AstronCodingPlan"
api_key = ""
models = ["xopglm52", "xopglm51", "xopdeepseekv4pro", "xopkimik26", "auto", "xopdeepseekv4flash"]
max_retries = 10000
retry_on_code = [10007, 10008, 10009, 10010, 10012, 10110, 10222, 10223, 11200, 11201, 11202, 11203, 11210, 10310]
key_mode = "passthrough"

[provider.openai]
thinking_effort = "xhigh"
base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/v2"

[provider.anthropic]
thinking_effort = "max"
base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/anthropic"

[provider.responses]
thinking_effort = "xhigh"
path_mode = "full"
base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/v1/responses"
"#;

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::var("MINI_PROXY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config.toml"));

    // 处理 -h / --help：先尝试读 config 获取真实 listen
    let args: Vec<String> = std::env::args().collect();
    // 处理 --example：把内嵌的配置模板导出为 config.example.toml（覆盖旧文件）
    if args.iter().any(|a| a == "--example") {
        let out = PathBuf::from("config.example.toml");
        std::fs::write(&out, EXAMPLE_CONFIG)?;
        println!("已导出配置模板到 {}", out.canonicalize().unwrap_or(out).display());
        return Ok(());
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        // 尝试从 config 读取 listen，失败则用默认值
        let listen = config::Config::load(&config_path)
            .map(|c| c.server.listen)
            .unwrap_or_else(|_| "127.0.0.1:7946".to_string());

        println!("mini-proxy - 简洁版 AI API 代理（同渠道自动重试）\n");
        println!("用法:");
        println!("  mini-proxy              运行服务（默认读 config.toml）");
        println!("  mini-proxy -h|--help    显示此帮助（含配置模板）");
        println!("  mini-proxy --example    导出配置模板到 config.example.toml（覆盖旧文件）");
        println!("  MINI_PROXY_CONFIG=xxx.toml mini-proxy   指定配置文件\n");
        println!("首次运行若未发现 config.toml，会生成默认配置并直接启动（无需填 key）。\n");
        println!("对外服务端点（当前 config listen={}）:", listen);
        println!("  完整路径（裸路径，无 /v1 /v2 /v3 前缀）：");
        println!("    POST http://{}/chat/completions  → OpenAI 协议", listen);
        println!("    POST http://{}/v1/messages       → Anthropic 协议", listen);
        println!("    POST http://{}/responses         → Response 协议", listen);
        println!("  通常填写（SDK base_url）：");
        println!("    http://{}   → 三种协议共用，按路径自动区分\n", listen);
        println!("===== 配置模板（config.toml）=====");
        print!("{}", EXAMPLE_CONFIG);
        return Ok(());
    }

    // 首次运行：config.toml 不存在 → 生成并直接启动（默认 passthrough 无需填 key）
    if !config_path.exists() {
        println!("未发现配置文件：{}", config_path.display());
        println!("已生成默认配置并启动，如需修改请编辑后重启。");
        std::fs::write(&config_path, EXAMPLE_CONFIG)?;
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
        if let Some(ep) = p.anthropic_endpoint() {
            tracing::info!(
                provider = %p.name,
                protocol = "anthropic",
                base_url = %ep.base_url,
                models = ?ep.models,
                max_retries = ep.max_retries,
                key_mode = ?ep.key_mode,
                "已加载渠道"
            );
        }
        if let Some(ep) = p.responses_endpoint() {
            tracing::info!(
                provider = %p.name,
                protocol = "responses",
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
    tracing::info!(listen = %cfg.server.listen, "服务启动完成");
    println!();
    println!("═══════════════════════════════════════════════════════════");
    println!("  mini-proxy 已启动，监听 {}", cfg.server.listen);
    println!();
    println!("  对外服务端点：");
    println!("    完整路径（裸路径，无 /v1 /v2 /v3 前缀）：");
    println!("      POST http://{}/chat/completions  → OpenAI 协议", cfg.server.listen);
    println!("      POST http://{}/v1/messages       → Anthropic 协议", cfg.server.listen);
    println!("      POST http://{}/responses         → Response 协议", cfg.server.listen);
    println!("    通常填写（SDK base_url）：");
    println!("      http://{}   → 三种协议共用，按路径自动区分", cfg.server.listen);
    println!("═══════════════════════════════════════════════════════════");
    axum::serve(listener, app).await?;
    Ok(())
}
