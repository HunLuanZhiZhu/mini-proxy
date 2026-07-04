# mini-proxy

简洁版 AI API 代理，单可执行文件，支持同渠道自动重试。

## 特性

- **三种协议透传**：OpenAI（`/v1`）、Anthropic（`/v2`）、Response（`/v3`）
- **自动重试**：HTTP 状态码范围 + 业务错误码双重判断，默认重试 10000 次
- **流式支持**：完整 SSE 透传，流中途错误也能检测并重试
- **API Key 双模式**：passthrough（透传客户端 Key）/ override（config 覆盖）
- **模型名映射**：客户端模型名 → 上游模型名
- **请求体清洗**：补全缺失字段、移除空内容项
- **单文件部署**：预编译 exe + 自动生成配置，开箱即用

## 快速开始

### Windows

```bash
# 下载 mini-proxy.exe，直接运行
mini-proxy.exe

# 首次运行自动生成 config.toml 并启动
# 默认配置使用讯飞星辰 MaaS Coding Plan，无需填 Key
```

### 从源码编译

```bash
cargo build --release
# 产物：target/release/mini-proxy.exe
```

### 配置

首次运行生成 `config.toml`，修改后重启生效。也可用 `--help` 查看完整配置模板：

```bash
mini-proxy.exe --help
```

## 对外端点

| 协议 | 完整路径 | SDK base_url |
|---|---|---|
| OpenAI | `POST http://<listen>/v1/chat/completions` | `http://<listen>/v1` |
| Anthropic | `POST http://<listen>/v2/v1/messages` | `http://<listen>/v2` |
| Response | `POST http://<listen>/v3/responses` | `http://<listen>/v3` |

路径按前缀路由：`/v1` → OpenAI，`/v2` → Anthropic，`/v3` → Response。

## 配置示例

```toml
[server]
listen = "127.0.0.1:7946"
clean_empty_content = true

[log]
level = "info"
format = "pretty"
to_stdout = true
to_file = "logs/proxy.log"

[[provider]]
name = "AstronCodingPlan"
api_key = ""
models = ["xopglm52", "xopglm51", "xopdeepseekv4pro", "xopkimik26"]
max_retries = 10000
retry_on_status = ["100-199", "300-399", "401-407", "409-499", "500-503", "505-523", "525-599"]
retry_on_code = [10007, 10008, 10009, 10010, 10012, 10110, 10222, 10223, 11200, 11201, 11202, 11203, 11210]
key_mode = "passthrough"

[provider.openai]
base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/v2"

[provider.anthropic]
base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/anthropic"

[provider.responses]
path_mode = "full"
base_url = "https://maas-coding-api.cn-huabei-1.xf-yun.com/v1/responses"
```

## 关键字段说明

### [server]

| 字段 | 说明 | 默认值 |
|---|---|---|
| `listen` | 本地监听地址 | `127.0.0.1:7946` |
| `clean_empty_content` | 清洗请求体：补全缺失 `type:"message"`，移除空 content 项 | `true` |

### [[provider]]

provider 级字段可被三种协议端点共用，endpoint 级同名字段覆盖之。

| 字段 | 说明 | 默认值 |
|---|---|---|
| `api_key` | API Key（仅 override 模式） | `""` |
| `models` | 支持的模型列表 | - |
| `model_map` | 模型名映射（客户端名 → 上游名） | 空（同名透传） |
| `max_retries` | 最大重试次数 | `10000` |
| `retry_on_status` | 可重试 HTTP 状态码范围 | 见下文 |
| `retry_on_code` | 可重试业务错误码 | 见下文 |
| `key_mode` | `passthrough` / `override` | `passthrough` |
| `path_mode` | `append` / `full` | `append` |

### 端点级配置

| 段 | 协议 | append 后缀 |
|---|---|---|
| `[provider.openai]` | OpenAI | `/chat/completions` |
| `[provider.anthropic]` | Anthropic | `/v1/messages` |
| `[provider.responses]` | Response | `/responses` |

## 重试机制

### HTTP 状态码（retry_on_status）

留空则使用默认范围（参考 new-api）：

```
100-199, 300-399, 401-407, 409-499, 500-503, 505-523, 525-599
```

永远不重试：`504`、`524`。

### 业务错误码（retry_on_code）

解析响应 body 中的 `error.code` 字段（支持流式 SSE 中的 `event: error`）。留空则使用讯飞默认码：

```
10007, 10008, 10009, 10010, 10012, 10110, 10222, 10223,
11200, 11201, 11202, 11203, 11210
```

### 流式错误检测

流式响应（SSE）会预读前几个事件检测是否含 `event: error`：
- 命中可重试业务码 → 重试（已读内容丢弃，重新请求）
- 遇到有效内容事件 → 停止预读，已读内容 + 剩余流一起转发给客户端

## API Key 模式

| 模式 | 行为 |
|---|---|
| `passthrough`（默认） | 保留客户端请求中的 Key，config 不存储不管理 |
| `override` | 用 config 的 `api_key` 覆盖客户端 Key（`api_key` 为空时回退到 passthrough） |

## 上游 URL 拼接

| 模式 | 行为 |
|---|---|
| `append`（默认） | `base_url` + 协议后缀（如 `/chat/completions`） |
| `full` | `base_url` 原样使用 |

## 日志

双输出：stdout（pretty 彩色）+ 文件（json 按天滚动）。

```toml
[log]
level = "info"           # trace | debug | info | warn | error
format = "pretty"        # pretty | json
to_stdout = true
to_file = "logs/proxy.log"
```

## 客户端配置示例

### Cursor

```
Override OpenAI Base URL: http://127.0.0.1:7946/v1
OpenAI API Key: 任意值（passthrough 模式填真实 Key）
```

### Claude Code

```json
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "你的-key",
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:7946/v2"
  }
}
```

### CodeX

```toml
[model_providers.xf-api]
base_url = "http://127.0.0.1:7946/v1"
```

## 技术栈

- Rust 2021 + Tokio
- axum（HTTP 服务）+ reqwest（HTTP 客户端，rustls）
- tracing（日志）+ serde/toml（配置）

## 许可

自用项目，无外部依赖。
