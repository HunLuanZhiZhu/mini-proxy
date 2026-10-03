<div align="center">

<img src="./assets/mini-proxy-banner.svg" width="100%" alt="mini-proxy — one local endpoint for multiple AI API protocols" />

<br/>

<a href="./READMEch.md"><img src="https://img.shields.io/badge/中文文档-READMEch.md-2563EB?style=for-the-badge" alt="Chinese README"/></a>
<a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust_2021-000000?style=for-the-badge&logo=rust&logoColor=white" alt="Rust 2021"/></a>
<img src="https://img.shields.io/badge/version-0.1.0-7C3AED?style=for-the-badge" alt="version 0.1.0"/>
<img src="https://img.shields.io/badge/SSE-streaming-059669?style=for-the-badge" alt="SSE streaming"/>

<br/><br/>

**A compact local AI API proxy with protocol passthrough, retries, model routing, request adaptation, and single-binary deployment.**

</div>

---

## Why mini-proxy?

Many AI coding clients speak different wire protocols even when they ultimately target similar model providers.

mini-proxy gives them a **single local base URL** and routes requests by path:

<table>
<tr>
<td width="33%" align="center"><b>OpenAI-style</b><br/><code>/chat/completions</code></td>
<td width="33%" align="center"><b>Anthropic-style</b><br/><code>/v1/messages</code></td>
<td width="33%" align="center"><b>Responses-style</b><br/><code>/responses</code></td>
</tr>
</table>

It is designed for local model gateways, coding-plan APIs, compatibility layers, and setups where you want one stable endpoint in front of several upstream services.

---

## Highlights

| Capability | What it does |
|---|---|
| 🔀 **Three protocol paths** | OpenAI Chat Completions, Anthropic Messages, and Responses API style traffic |
| ♻️ **Automatic retry** | Retries by HTTP status range and provider business error code |
| 🌊 **SSE streaming** | Preserves streaming responses and can detect retryable errors during early stream events |
| 🔑 **Two API-key modes** | Client-key passthrough or configuration override |
| 🗺️ **Model mapping** | Maps client-facing model IDs to upstream model IDs |
| 🧹 **Request cleanup** | Normalizes selected request fields and removes empty content entries |
| 🧠 **Reasoning-effort injection** | Can force protocol-specific reasoning effort or leave client values untouched |
| 🧩 **Provider inheritance** | Shared provider settings with per-endpoint overrides |
| 🪪 **Header injection** | Adds configured headers only when the client did not already provide them |
| 🧵 **OpenCode session support** | Optional automatic <code>x-opencode-session</code> generation / propagation |
| 📦 **Single executable** | Build once, run directly; first launch can generate configuration |
| 🪵 **Structured logging** | Console + rolling file logs through <code>tracing</code> |

---

## Quick start

### Windows

Download or build <code>mini-proxy.exe</code>, then run:

~~~powershell
mini-proxy.exe
~~~

On first launch, mini-proxy can create <code>config.toml</code> and start with the configured defaults.

### Build from source

~~~bash
cargo build --release
~~~

Binary:

~~~text
target/release/mini-proxy
~~~

On Windows:

~~~text
target/release/mini-proxy.exe
~~~

### Inspect configuration help

~~~bash
mini-proxy --help
~~~

---

## Request flow

~~~text
Cursor / Claude Code / Codex / other clients
                    │
                    ▼
          http://127.0.0.1:7946
                    │
        ┌───────────┼───────────┐
        ▼           ▼           ▼
 /chat/completions /v1/messages /responses
      OpenAI        Anthropic    Responses
        │           │           │
        └──── provider routing ──┘
                    │
          model map / headers
        retry / key handling
        reasoning adaptation
                    │
                    ▼
             upstream APIs
~~~

---

## Public endpoints

| Protocol | Request path | SDK base URL |
|---|---|---|
| OpenAI-style | <code>POST http://&lt;listen&gt;/chat/completions</code> | <code>http://&lt;listen&gt;</code> |
| Anthropic-style | <code>POST http://&lt;listen&gt;/v1/messages</code> | <code>http://&lt;listen&gt;</code> |
| Responses-style | <code>POST http://&lt;listen&gt;/responses</code> | <code>http://&lt;listen&gt;</code> |

Default listen address:

~~~text
127.0.0.1:7946
~~~

The three protocols share the same base URL. mini-proxy distinguishes them by the incoming request path.

---

## Configuration

A minimal example:

~~~toml
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
name = "ExampleProvider"
api_key = ""
models = ["model-a", "model-b"]
max_retries = 10000
key_mode = "passthrough"
thinking_effort = "passthrough"

[provider.openai]
base_url = "https://example.com/v1"

[provider.anthropic]
base_url = "https://example.com"

[provider.responses]
base_url = "https://example.com/v1"
~~~

The repository's <code>config.toml</code> contains a much more complete field reference and working provider examples.

### Core provider fields

| Field | Meaning | Typical / default behavior |
|---|---|---|
| <code>name</code> | Provider label used in logs | required |
| <code>api_key</code> | Key used in override mode | empty |
| <code>models</code> | Client-visible model IDs handled by this provider | provider-specific |
| <code>model_map</code> | Client model ID → upstream model ID | same-name passthrough when absent |
| <code>max_retries</code> | Maximum retries on the same provider/model | <code>10000</code> |
| <code>retry_on_status</code> | Retryable HTTP codes / ranges | built-in defaults when omitted |
| <code>retry_on_code</code> | Retryable provider business error codes | built-in defaults when omitted |
| <code>key_mode</code> | <code>passthrough</code> or <code>override</code> | <code>passthrough</code> |
| <code>path_mode</code> | <code>append</code> or <code>full</code> | <code>append</code> |
| <code>thinking_effort</code> | Force a reasoning level or pass the client value through | protocol-dependent default when omitted |
| <code>is_opencode</code> | Enable automatic OpenCode session header handling | <code>false</code> |
| <code>headers</code> | Static headers inserted only when absent from the request | empty |

Endpoint sections such as <code>[provider.openai]</code>, <code>[provider.anthropic]</code>, and <code>[provider.responses]</code> inherit provider-level values and may override them.

---

## Retry behavior

### HTTP status retry

When <code>retry_on_status</code> is omitted, mini-proxy uses its built-in retry ranges. The current configuration documentation lists ranges covering most transient / provider-side failures while explicitly excluding <code>504</code> and <code>524</code> from retry.

### Provider error-code retry

mini-proxy can inspect <code>error.code</code> in the response body and retry selected provider-specific business errors.

### Streaming retry

For SSE responses, mini-proxy pre-reads the early stream events:

1. a retryable <code>event: error</code> can trigger a fresh upstream attempt;
2. once valid content begins, buffered events are forwarded together with the remainder of the stream.

This avoids committing a broken early stream to the client when the upstream failure is still recoverable.

---

## API key modes

| Mode | Behavior |
|---|---|
| <code>passthrough</code> | Preserve the client's key; mini-proxy does not need to own it |
| <code>override</code> | Replace the client key with <code>api_key</code> from configuration; an empty configured key falls back to passthrough |

---

## Upstream URL modes

| Mode | Behavior |
|---|---|
| <code>append</code> | <code>base_url</code> + protocol suffix, such as <code>/chat/completions</code> |
| <code>full</code> | Use <code>base_url</code> exactly as configured |

This is useful when different providers expose either a common API root or a fully specified endpoint.

---

## Client examples

### Cursor — OpenAI-style endpoint

~~~text
Override OpenAI Base URL: http://127.0.0.1:7946
OpenAI API Key: <your key>
Model: <configured model id>
~~~

### Claude Code — Anthropic-style endpoint

~~~json
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "<your key>",
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:7946",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    "API_TIMEOUT_MS": "600000",
    "ANTHROPIC_MODEL": "<configured model id>"
  }
}
~~~

### Codex — Responses-style endpoint

<code>auth.json</code>:

~~~json
{
  "OPENAI_API_KEY": "<your key>"
}
~~~

Example client configuration:

~~~toml
model_provider = "mini-proxy"
model = "<configured model id>"
disable_response_storage = true
preferred_auth_method = "apikey"

[model_providers.mini-proxy]
name = "mini-proxy"
base_url = "http://127.0.0.1:7946"
wire_api = "responses"
~~~

---

## Logging

Example:

~~~toml
[log]
level = "info"
format = "pretty"
to_stdout = true
to_file = "logs/proxy.log"
rotate_size_mb = 50
rotate_keep = 7
~~~

The project uses <code>tracing</code> / <code>tracing-subscriber</code> and supports console output plus rolling file logs.

---

## Tech stack

<div align="center">

<img src="https://img.shields.io/badge/Rust_2021-000000?style=flat-square&logo=rust&logoColor=white" />
<img src="https://img.shields.io/badge/Tokio-async_runtime-1F6FEB?style=flat-square" />
<img src="https://img.shields.io/badge/axum-HTTP_server-6B7280?style=flat-square" />
<img src="https://img.shields.io/badge/reqwest-HTTP_client-0EA5E9?style=flat-square" />
<img src="https://img.shields.io/badge/rustls-TLS-059669?style=flat-square" />
<img src="https://img.shields.io/badge/serde-config_&_JSON-F59E0B?style=flat-square" />
<img src="https://img.shields.io/badge/tracing-logging-8B5CF6?style=flat-square" />

</div>

---

## Project status

mini-proxy is currently a **personal-use project**. Configuration defaults and provider examples may evolve with the upstream services it is used against.

For Chinese documentation, see **[READMEch.md](./READMEch.md)**.
