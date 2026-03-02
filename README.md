# llmperf-rs

> ⚠️ **此项目由 AI 生成**，作者测试并验证了在 Windows 以及 Ubuntu 环境下的可用性

LLM API Testing CLI Tool - 用于测试 LLM API 性能的命令行工具。
支持并发测试最大输出，支持json格式输出，支持通过配置文件快速测试，支持直接chat测试，适合在无用户界面或ssh环境下使用，也很适合在CI流程中使用。

> 📖 [English README](./README_en.md)

## 功能特性

- 支持 OpenAI API 兼容的 LLM API 测试
- 实时时间片采样，终端表格动态显示每个请求的吞吐量
- 并发请求测试，系统级吞吐量统计
- 多种上下文大小测试（支持范围格式）
- 交互式聊天模式，支持从文件加载 prompt
- 支持 reasoning 模型（如 GLM-5 的 `reasoning_content` 字段）
- 环境监控（CPU、内存、GPU 信息）
- 中英文双语支持
- Ctrl+C 优雅退出，安全中断测试
- 配置保存/恢复，支持测试场景复用

## 编译

### 环境要求

- Rust 1.70+
- Cargo

### 编译步骤

```bash
cd llmperf-rs
cargo build --release
# 可执行文件: target/release/llmperf-rs
```

## 配置

支持三种配置方式，优先级从高到低：

1. **命令行参数** `--base-url` / `--api-key`
2. **配置文件** `llmperf-rs.yaml`（当前目录），回退到 `/etc/llmperf-rs/llmperf-rs.yaml`（系统级）
3. **环境变量** `OPENAI_BASE_URL` / `OPENAI_API_KEY`

首次运行时，若配置文件不存在：
- 如果提供了命令行参数或环境变量，提示是否保存配置文件
- 否则交互式引导创建配置文件

### 配置文件参数

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `base_url` | String | 是 | - | API 基础地址 |
| `api_key` | String | 是 | - | API 密钥 |
| `model` | String | 否 | `gpt-4` | 默认模型 (也支持 `default_model`) |
| `lang` | String | 否 | `en` | 输出语言 `zh` / `en` |
| `time_slice_interval` | Float | 否 | `3.0` | 时间片采样间隔（秒） |

#### test 子配置

| 参数 | 类型 | 说明 |
|------|------|------|
| `concurrent` | Integer | 并发请求数 |
| `context` | String | 上下文大小 |
| `max_tokens` | Integer | 最大生成 token 数 |
| `env_monitor` | Boolean | 是否输出环境信息 |
| `time_slice` | Float | 时间片采样间隔 |

#### chat 子配置

| 参数 | 类型 | 说明 |
|------|------|------|
| `max_tokens` | Integer | 每次回复最大 token 数 |
| `prompt` | String | 初始 prompt |

### 配置示例

```yaml
base_url: "https://api.openai.com/v1"
api_key: "your-api-key-here"
model: "gpt-4"
lang: "zh"
time_slice_interval: 3.0

test:
  concurrent: 4
  context: "4096"
  max_tokens: 128

chat:
  max_tokens: 1024
  prompt: "你是一个助手"
```

## 使用方法

### 全局选项

```
--config <FILE>          配置文件路径 (默认: llmperf-rs.yaml)
--base-url <URL>         API 地址 (覆盖配置文件，env: OPENAI_BASE_URL)
--api-key <KEY>          API 密钥 (覆盖配置文件，env: OPENAI_API_KEY)
--json                   以 JSON 格式输出结果（抑制进度显示）
--lang <LANG>            输出语言 (zh/en)，覆盖自动检测
--save-config <FILE>     保存当前配置到文件并退出
```

### 性能测试 (test)

> 不指定子命令时：
> - 若配置文件中只有 `chat` 子配置，进入 chat 模式
> - 其他情况默认以 test 模式运行（并发 1，上下文 1024，最大 256 token）

```bash
llmperf-rs test [OPTIONS]
```

| 选项 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--concurrent <NUM>` | `-j` | `1` | 并发请求数 |
| `--context <SIZES>` | `-c` | `1024` | 上下文大小，支持 `start:step:end` 范围格式 |
| `--max-tokens <NUM>` | - | `256` | 最大生成 token 数 |
| `--model <MODEL>` | `-m` | 配置文件默认值 | 测试模型 |
| `--env-monitor` | `-e` | `false` | 输出环境信息 |
| `--time-slice <SECS>` | - | `3.0` | 时间片采样间隔（秒） |
| `--json` | - | `false` | 以 JSON 格式输出结果 |
| `--lang <LANG>` | - | 自动检测 | 输出语言 (zh/en) |
| `--save-config <FILE>` | - | - | 保存当前配置到文件并退出 |

**示例：**

```bash
# 默认测试
llmperf-rs test

# 4 并发，依次测试1024、2048、3072、4096 4段上下文长度下数据
llmperf-rs test -j 4 -c 1024:1024:4096

# 指定模型和 API 地址
llmperf-rs test -m qwen-plus --base-url https://api.example.com/v1 --api-key sk-xxx

# 使用环境变量
export OPENAI_BASE_URL=https://api.example.com/v1
export OPENAI_API_KEY=sk-xxx
llmperf-rs test -j 3 -c 4096

# JSON 格式输出（适合程序化处理）
llmperf-rs test -j 2 -c 2048 --json

# 保存配置
llmperf-rs test -j 4 -c 4096 --save-config test.yaml

# 从配置文件恢复测试场景
llmperf-rs --config test.yaml
```

### 交互式聊天 (chat)

```bash
llmperf-rs chat [OPTIONS]
```

| 选项 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--model <MODEL>` | `-m` | 配置文件默认值 | 聊天模型 |
| `--prompt <TEXT>` | `-p` | - | 初始 prompt，支持 `@filepath` 从文件读取 |
| `--max-tokens <NUM>` | - | `1024` | 每次回复最大 token 数 |
| `--lang <LANG>` | - | 自动检测 | 输出语言 (zh/en) |
| `--save-config <FILE>` | - | - | 保存当前配置到文件并退出 |

**示例：**

```bash
# 默认聊天
llmperf-rs chat

# 指定模型
llmperf-rs chat -m gpt-4

# 带初始 prompt
llmperf-rs chat -p "请解释量子计算"

# 从文件读取 prompt
llmperf-rs chat -p @prompt.txt

# 保存配置
llmperf-rs chat -p "hello" --save-config chat.yaml

# 从配置文件恢复聊天场景（自动执行保存的 prompt）
llmperf-rs --config chat.yaml
```

聊天模式内置命令：`/clear` 清空历史、`/exit` 退出、`/help` 帮助。

## 输出说明

### 实时显示

测试运行时，终端会实时显示每个请求的状态表格：

```
  Elapsed 5.2s
   #    |    Status    |   Tput(t/s)   |  Out Toks  |   Time(s)
--------------------------------------------------------------
   1    |    Done      |         58.3  |       256  |       4.4
   2    |    Decode    |         61.2  |       180  |       3.1
   3    |  ⠹ Prefill   |            -  |          - |       0.3
--------------------------------------------------------------
 System    Prefill: 2280 input t/s | Decode: 119.5 output t/s
```

### 最终统计

测试完成后输出系统级吞吐量（总 token / 墙钟时间），而非单请求平均值。

## 许可证

MIT License
