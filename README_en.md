# llmperf-rs

> ⚠️ **This project is AI-generated**, and the author has tested and verified its usability on Windows and Ubuntu.

## Features

- Support for OpenAI API-compatible LLM API testing
- Real-time time-slice sampling with dynamic terminal table showing throughput per request
- Concurrent request testing with system-level throughput statistics
- Multiple context size testing (supports range format)
- Interactive chat mode with support for loading prompts from files
- Support for reasoning models (e.g., GLM-5's `reasoning_content` field)
- Environment monitoring (CPU, memory, GPU info)
- Bilingual support (Chinese/English)
- Graceful Ctrl+C exit with safe test interruption
- Configuration save/restore for test scenario reuse

## Build

### Requirements

- Rust 1.70+
- Cargo

### Build Steps

```bash
cd llmperf-rs
cargo build --release
# Executable: target/release/llmperf-rs
```

## Configuration

Supports three configuration methods, priority from high to low:

1. **Command-line arguments** `--base-url` / `--api-key`
2. **Config file** `llmperf-rs.yaml` (current directory), fallback to `/etc/llmperf-rs/llmperf-rs.yaml` (system-level)
3. **Environment variables** `OPENAI_BASE_URL` / `OPENAI_API_KEY`

On first run, if config file doesn't exist:
- If command-line arguments or environment variables are provided, prompt to save config file
- Otherwise, interactively guide to create config file

### Config File Parameters

| Parameter | Type | Required | Default | Description |
|------------|------|----------|---------|-------------|
| `base_url` | String | Yes | - | API base URL |
| `api_key` | String | Yes | - | API key |
| `model` | String | No | `gpt-4` | Default model (also supports `default_model`) |
| `lang` | String | No | `en` | Output language `zh` / `en` |
| `time_slice_interval` | Float | No | `3.0` | Time slice sampling interval (seconds) |

#### test sub-config

| Parameter | Type | Description |
|------------|------|-------------|
| `concurrent` | Integer | Number of concurrent requests |
| `context` | String | Context size |
| `max_tokens` | Integer | Max tokens to generate |
| `env_monitor` | Boolean | Whether to output environment info |
| `time_slice` | Float | Time slice sampling interval |

#### chat sub-config

| Parameter | Type | Description |
|------------|------|-------------|
| `max_tokens` | Integer | Max tokens per response |
| `prompt` | String | Initial prompt |

### Config Example

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
  prompt: "You are an assistant"
```

## Usage

### Global Options

```
--config <FILE>          Config file path (default: llmperf-rs.yaml)
--base-url <URL>         API URL (overrides config file, env: OPENAI_BASE_URL)
--api-key <KEY>          API key (overrides config file, env: OPENAI_API_KEY)
--json                   Output results in JSON format (suppresses progress display)
--lang <LANG>            Output language (zh/en), overrides auto-detection
--save-config <FILE>     Save current config to file and exit
```

### Performance Testing (test)

> When no subcommand is specified:
> - If config file only has `chat` sub-config, enter chat mode
> - Otherwise, run in test mode by default (concurrent 1, context 1024, max 256 tokens)

```bash
llmperf-rs test [OPTIONS]
```

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--concurrent <NUM>` | `-j` | `1` | Number of concurrent requests |
| `--context <SIZES>` | `-c` | `1024` | Context size, supports `start:step:end` range format |
| `--max-tokens <NUM>` | - | `256` | Max tokens to generate |
| `--model <MODEL>` | `-m` | Config default | Test model |
| `--env-monitor` | `-e` | `false` | Output environment info |
| `--time-slice <SECS>` | - | `3.0` | Time slice sampling interval (seconds) |
| `--json` | - | `false` | Output results in JSON format |
| `--lang <LANG>` | - | Auto-detect | Output language (zh/en) |
| `--save-config <FILE>` | - | - | Save current config to file and exit |

**Examples:**

```bash
# Default test
llmperf-rs test

# 4 concurrent, multiple context sizes
llmperf-rs test -j 4 -c 1024:1024:4096

# Specify model and API URL
llmperf-rs test -m qwen-plus --base-url https://api.example.com/v1 --api-key sk-xxx

# Use environment variables
export OPENAI_BASE_URL=https://api.example.com/v1
export OPENAI_API_KEY=sk-xxx
llmperf-rs test -j 3 -c 4096

# JSON output (suitable for programmatic processing)
llmperf-rs test -j 2 -c 2048 --json

# Save config
llmperf-rs test -j 4 -c 4096 --save-config test.yaml

# Restore test scenario from config file
llmperf-rs --config test.yaml
```

### Interactive Chat (chat)

```bash
llmperf-rs chat [OPTIONS]
```

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--model <MODEL>` | `-m` | Config default | Chat model |
| `--prompt <TEXT>` | `-p` | - | Initial prompt, supports `@filepath` to read from file |
| `--max-tokens <NUM>` | - | `1024` | Max tokens per response |
| `--lang <LANG>` | - | Auto-detect | Output language (zh/en) |
| `--save-config <FILE>` | - | - | Save current config to file and exit |

**Examples:**

```bash
# Default chat
llmperf-rs chat

# Specify model
llmperf-rs chat -m gpt-4

# With initial prompt
llmperf-rs chat -p "Explain quantum computing"

# Read prompt from file
llmperf-rs chat -p @prompt.txt

# Save config
llmperf-rs chat -p "hello" --save-config chat.yaml

# Restore chat scenario from config file (auto-executes saved prompt)
llmperf-rs --config chat.yaml
```

Chat mode built-in commands: `/clear` clear history, `/exit` exit, `/help` help.

## Output Description

### Real-time Display

During test run, the terminal displays a real-time status table for each request:

```
  Elapsed 5.2s
   #    |    Status    |   Tput(t/s)   |  Out Toks  |   Time(s)
--------------------------------------------------------------
   1    |    Done      |         58.3  |       256  |       4.4
   2    |    Decode    |         61.2  |       180  |       3.3
   3    |  ⠹ Prefill   |            -  |          - |       0.3
--------------------------------------------------------------
 System    Prefill: 2280 input t/s | Decode: 119.5 output t/s
```

### Final Statistics

After test completes, outputs system-level throughput (total tokens / wall clock time), not per-request average.

## License

MIT License
