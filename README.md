# llmperf

LLM API Testing CLI Tool - 用于测试 LLM API 性能的命令行工具。

## 功能特性

- 支持 OpenAI API 兼容的 LLM API 测试
- 实时时间片采样，终端表格动态显示每个请求的吞吐量
- 并发请求测试，系统级吞吐量统计
- 多种上下文大小测试（支持范围格式）
- 交互式聊天模式，支持从文件加载 prompt
- 支持 reasoning 模型（如 GLM-5 的 `reasoning_content` 字段）
- 环境监控（CPU、内存、GPU 信息）
- 中英文双语支持

## 编译

### 环境要求

- Rust 1.70+
- Cargo

### 编译步骤

```bash
cd llmperf
cargo build --release
# 可执行文件: target/release/llm-api-tester
```

## 配置

支持三种配置方式，优先级从高到低：

1. **命令行参数** `--base-url` / `--api-key`
2. **配置文件** `config.yaml`
3. **环境变量** `OPENAI_BASE_URL` / `OPENAI_API_KEY`

首次运行时，若配置文件不存在：
- 如果提供了命令行参数或环境变量，自动写入 `config.yaml`
- 否则交互式引导创建配置文件

### 配置文件参数

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `base_url` | String | 是 | - | API 基础地址 |
| `api_key` | String | 是 | - | API 密钥 |
| `default_model` | String | 否 | `gpt-4` | 默认模型 |
| `lang` | String | 否 | `en` | 输出语言 `zh` / `en` |
| `time_slice_interval` | Float | 否 | `3.0` | 时间片采样间隔（秒） |

### 配置示例

```yaml
base_url: "https://api.openai.com/v1"
api_key: "your-api-key-here"
default_model: "gpt-4"
lang: "zh"
time_slice_interval: 3.0
```

## 使用方法

### 全局选项

```
--config <FILE>          配置文件路径 (默认: config.yaml)
--base-url <URL>         API 地址 (覆盖配置文件，env: OPENAI_BASE_URL)
--api-key <KEY>          API 密钥 (覆盖配置文件，env: OPENAI_API_KEY)
```

### 性能测试 (test)

```bash
llm-api-tester test [OPTIONS]
```

| 选项 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--concurrent <NUM>` | `-j` | `1` | 并发请求数 |
| `--context <SIZES>` | `-c` | `1024` | 上下文大小，支持 `start:step:end` 范围格式 |
| `--max-tokens <NUM>` | - | `256` | 最大生成 token 数 |
| `--model <MODEL>` | `-m` | 配置文件默认值 | 测试模型 |
| `--env-monitor` | `-e` | `false` | 输出环境信息 |
| `--time-slice <SECS>` | - | `3.0` | 时间片采样间隔（秒） |

**示例：**

```bash
# 默认测试
llm-api-tester test

# 4 并发，多上下文大小
llm-api-tester test -j 4 -c 1024:1024:4096

# 指定模型和 API 地址
llm-api-tester test -m qwen-plus --base-url https://api.example.com/v1 --api-key sk-xxx

# 使用环境变量
export OPENAI_BASE_URL=https://api.example.com/v1
export OPENAI_API_KEY=sk-xxx
llm-api-tester test -j 3 -c 4096
```

### 交互式聊天 (chat)

```bash
llm-api-tester chat [OPTIONS]
```

| 选项 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--model <MODEL>` | `-m` | 配置文件默认值 | 聊天模型 |
| `--prompt <TEXT>` | `-p` | - | 初始 prompt，支持 `@filepath` 从文件读取 |

**示例：**

```bash
# 默认聊天
llm-api-tester chat

# 指定模型
llm-api-tester chat -m gpt-4

# 带初始 prompt
llm-api-tester chat -p "请解释量子计算"

# 从文件读取 prompt
llm-api-tester chat -p @prompt.txt
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
