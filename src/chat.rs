use crate::client::{ApiClient, ChatMessage, ChatStreamResult};
use crate::config::Config;
use std::io::{self, Write};

pub fn run_chat(config: Config, model: Option<String>, initial_prompt: Option<String>, max_tokens: u32) {
    let model = model.unwrap_or(config.default_model);
    let client = ApiClient::new(config.base_url, config.api_key);
    let lang = config.lang;

    let (help_cmd, help_clear, help_exit, help_error, lbl_user, lbl_ai,
         lbl_prefill, lbl_decode, lbl_stats) = if lang == "zh" {
        ("帮助", "清空对话历史", "退出聊天", "错误",
         "用户", "AI", "Prefill", "Decode", "统计信息")
    } else {
        ("help", "Clear conversation", "Exit chat", "Error",
         "You", "AI", "Prefill", "Decode", "Statistics")
    };

    println!("\n=== Chat Mode ===");
    println!("Model: {}", model);
    println!("Commands:");
    println!("  /clear - {}", help_clear);
    println!("  /exit  - {}", help_exit);
    println!("  /help  - {}", help_cmd);
    println!("-----------\n");

    let runtime = tokio::runtime::Runtime::new().expect("Failed to create runtime");
    let mut messages: Vec<(String, String)> = Vec::new();
    let mut first_input = initial_prompt;

    loop {
        let input = if let Some(prompt) = first_input.take() {
            println!("{} {}", lbl_user, prompt);
            prompt
        } else {
            print!("\n{} ", lbl_user);
            io::stdout().flush().unwrap();
            let mut buf = String::new();
            io::stdin().read_line(&mut buf).unwrap();
            buf.trim().to_string()
        };

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
            client.chat_streaming(&model, chat_messages, max_tokens, |chunk| {
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