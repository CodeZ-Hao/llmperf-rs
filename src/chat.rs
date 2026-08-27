use crate::client::{ApiClient, ChatMessage, ChatStreamResult};
use crate::config::{Config, DEFAULT_MODEL};
use serde_json::json;
use std::io::{self, Write};
use std::time::Duration;

pub fn run_chat(config: Config, model: Option<String>, initial_prompt: Option<String>, max_tokens: u32, json_output: bool, enable_thinking: bool) {
    let model = model.or(config.model);
    let client = ApiClient::new(
        config.base_url.unwrap(),
        config.api_key.unwrap_or_default(),
        config.timeout.map(Duration::from_secs),
    );
    let lang = config.lang;

    let runtime = tokio::runtime::Runtime::new().expect("Failed to create runtime");

    // Resolve model: explicit > first model from the API > gpt-4 fallback
    let model = match model {
        Some(m) => m,
        None => match runtime.block_on(client.fetch_first_model()) {
            Some(m) => m,
            None => {
                eprintln!("Warning: no model configured and failed to fetch models from API, falling back to \"{}\"", DEFAULT_MODEL);
                DEFAULT_MODEL.to_string()
            }
        },
    };

    // One-shot mode: -p given or --json (non-interactive, suitable for scripting)
    if initial_prompt.is_some() || json_output {
        let prompt_text = match initial_prompt {
            Some(p) => p,
            None => {
                eprintln!("Error: --json requires a prompt (use -p or set chat.prompt in config)");
                std::process::exit(1);
            }
        };
        run_one_shot(&client, &model, &prompt_text, max_tokens, &lang, json_output, &runtime, enable_thinking);
        return;
    }

    // Interactive mode
    let (help_cmd, help_clear, help_exit, help_error, help_think, lbl_user, lbl_ai,
         lbl_prefill, lbl_decode, lbl_stats) = if lang == "zh" {
        ("帮助", "清空对话历史", "退出聊天", "错误", "开关思考",
         "用户", "AI", "Prefill", "Decode", "统计信息")
    } else {
        ("help", "Clear conversation", "Exit chat", "Error", "Toggle thinking",
         "You", "AI", "Prefill", "Decode", "Statistics")
    };

    println!("\n=== Chat Mode ===");
    println!("Model: {}", model);
    let (lbl_thinking, state) = if lang == "zh" {
        ("思考", if enable_thinking { "开启" } else { "关闭" })
    } else {
        ("Thinking", if enable_thinking { "on" } else { "off" })
    };
    println!("{}: {}", lbl_thinking, state);
    println!("Commands:");
    println!("  /clear - {}", help_clear);
    println!("  /exit  - {}", help_exit);
    println!("  /help  - {}", help_cmd);
    println!("  /think - {}", help_think);
    println!("-----------\n");

    let mut messages: Vec<(String, String)> = Vec::new();
    let mut enable_thinking = enable_thinking;

    loop {
        print!("\n{} ", lbl_user);
        io::stdout().flush().unwrap();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).unwrap();
        let input = buf.trim().to_string();

        if input.is_empty() {
            continue;
        }

        if input.starts_with('/') {
            match input.as_str() {
                "/clear" => {
                    messages.clear();
                    println!("{}", if lang == "zh" { "对话历史已清空" } else { "Conversation cleared" });
                    continue;
                }
                "/exit" | "/quit" => {
                    println!("{}", if lang == "zh" { "退出聊天模式" } else { "Exiting chat mode" });
                    break;
                }
                "/help" => {
                    println!("Commands:");
                    println!("  /clear - {}", help_clear);
                    println!("  /exit  - {}", help_exit);
                    println!("  /help  - {}", help_cmd);
                    println!("  /think - {}", help_think);
                    continue;
                }
                "/think" => {
                    // Toggle thinking state
                    enable_thinking = !enable_thinking;
                    let (lbl_thinking, state) = if lang == "zh" {
                        ("思考", if enable_thinking { "开启" } else { "关闭" })
                    } else {
                        ("Thinking", if enable_thinking { "on" } else { "off" })
                    };
                    println!("{}: {}", lbl_thinking, state);
                    continue;
                }
                _ => {
                    println!("{}: {}. {} /help {}",
                        if lang == "zh" { "未知命令" } else { "Unknown command" },
                        input,
                        if lang == "zh" { "使用" } else { "Use" },
                        if lang == "zh" { "查看可用命令" } else { "for available commands" }
                    );
                    continue;
                }
            }
        }

        messages.push(("user".to_string(), input));

        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|(role, content)| ChatMessage {
                role: role.clone(),
                content: content.clone(),
            })
            .collect();

        print!("\n{} ", lbl_ai);
        io::stdout().flush().unwrap();

        let result = runtime.block_on(
            client.chat_streaming(&model, chat_messages, max_tokens, enable_thinking, |chunk| {
                print!("{}", chunk);
                io::stdout().flush().unwrap();
            })
        );

        match result {
            Ok(chat_result) => {
                let response = &chat_result.content;
                if response.is_empty() {
                    println!("(empty response)");
                }
                messages.push(("assistant".to_string(), response.clone()));

                print_stats(
                    &chat_result, &lang,
                    lbl_stats, lbl_prefill, lbl_decode,
                );
            }
            Err(e) => {
                println!("{}: {}", help_error, e);
            }
        }
    }
}

/// One-shot chat: stream the response (unless JSON), then print throughput stats
/// (or the JSON result). No interactive loop.
fn run_one_shot(
    client: &ApiClient,
    model: &str,
    prompt_text: &str,
    max_tokens: u32,
    lang: &str,
    json_output: bool,
    runtime: &tokio::runtime::Runtime,
    enable_thinking: bool,
) {
    let (lbl_user, lbl_ai, lbl_stats, lbl_prefill, lbl_decode) = if lang == "zh" {
        ("用户", "AI", "统计信息", "Prefill", "Decode")
    } else {
        ("You", "AI", "Statistics", "Prefill", "Decode")
    };

    if !json_output {
        println!("\n{} {}", lbl_user, prompt_text);
        print!("{} ", lbl_ai);
        io::stdout().flush().unwrap();
    }

    let chat_messages = vec![ChatMessage {
        role: "user".to_string(),
        content: prompt_text.to_string(),
    }];

    let result = runtime.block_on(
        client.chat_streaming(model, chat_messages, max_tokens, enable_thinking, |chunk| {
            if !json_output {
                print!("{}", chunk);
                io::stdout().flush().unwrap();
            }
        })
    );

    match result {
        Ok(chat_result) => {
            if json_output {
                let output = json!({
                    "model": model,
                    "prompt": prompt_text,
                    "content": chat_result.content,
                    "prompt_tokens": chat_result.prompt_tokens,
                    "completion_tokens": chat_result.completion_tokens,
                    "prefill_tok_per_sec": round2(chat_result.prefill_tps.unwrap_or(0.0)),
                    "decode_tok_per_sec": round2(chat_result.decode_tps.unwrap_or(0.0)),
                    "total_duration_secs": round2(chat_result.total_duration_secs),
                });
                println!("{}", serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string()));
            } else {
                if chat_result.content.is_empty() {
                    println!("(empty response)");
                }
                print_stats(&chat_result, lang, lbl_stats, lbl_prefill, lbl_decode);
            }
        }
        Err(e) => {
            if json_output {
                let output = json!({
                    "error": e,
                    "model": model,
                    "prompt": prompt_text,
                });
                println!("{}", serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string()));
                std::process::exit(1);
            } else {
                let help_error = if lang == "zh" { "错误" } else { "Error" };
                println!("{}: {}", help_error, e);
            }
        }
    }
}

fn print_stats(
    result: &ChatStreamResult, lang: &str,
    lbl_stats: &str, lbl_prefill: &str, lbl_decode: &str,
) {
    let prompt_tokens = result.prompt_tokens.unwrap_or(0);
    let completion_tokens = result.completion_tokens.unwrap_or(0);
    let prefill_tps = result.prefill_tps.unwrap_or(0.0);
    let decode_tps = result.decode_tps.unwrap_or(0.0);
    let tok_unit = if lang == "zh" { "tokens/s" } else { "tok/s" };

    if completion_tokens > 0 || prompt_tokens > 0 {
        println!("\n--- {} ---", lbl_stats);
        if prompt_tokens > 0 {
            println!("{}: {} | {}: {:.2} {}",
                lbl_prefill, prompt_tokens, lbl_prefill, prefill_tps, tok_unit);
        }
        if completion_tokens > 0 {
            println!("{}: {} | {}: {:.2} {}",
                lbl_decode, completion_tokens, lbl_decode, decode_tps, tok_unit);
        }
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
