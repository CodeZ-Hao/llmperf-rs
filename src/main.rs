mod client;
mod config;
mod env_monitor;
mod formatter;
mod test_runner;
mod chat;
mod live_display;
mod utils;

use clap::{Parser, Subcommand, ValueHint, CommandFactory, FromArgMatches};
use client::ApiClient;
use config::{Config, CliOverrides, TestConfig, ChatConfig};
use env_monitor::EnvMonitor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Parser, Debug)]
#[command(name = "llmperf-rs")]
#[command(about = "OpenAI API Testing CLI Tool", long_about = None)]
struct Cli {
    /// Configuration file path
    #[arg(long, value_hint = ValueHint::FilePath, default_value = "llmperf-rs.yaml")]
    config: PathBuf,

    /// API base URL (overrides config and env var)
    #[arg(long, env = "OPENAI_BASE_URL")]
    base_url: Option<String>,

    /// API key (overrides config and env var)
    #[arg(long, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    /// Model to use
    #[arg(short, long)]
    model: Option<String>,

    /// Output results as JSON (suppresses progress output)
    #[arg(long)]
    json: bool,

    /// Override system language detection (zh/en)
    #[arg(long)]
    lang: Option<String>,

    /// Save current effective config to file and exit
    #[arg(long, value_hint = ValueHint::FilePath)]
    save_config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run API performance tests
    Test {
        /// Concurrent requests
        #[arg(short = 'j', long, default_value = "1")]
        concurrent: usize,

        /// Context sizes (single value, or start:step:end range format)
        #[arg(short = 'c', long, default_value = "1024")]
        context: String,

        /// Max tokens to generate
        #[arg(long, default_value = "256")]
        max_tokens: u32,

        /// Enable environment monitoring
        #[arg(short, long)]
        env_monitor: bool,

        /// Time slice interval in seconds for real-time display
        #[arg(long)]
        time_slice: Option<f64>,

        // --- inherited base params (subcommand overrides base) ---

        /// Model to test (overrides base -m)
        #[arg(short, long)]
        model: Option<String>,

        /// API base URL (overrides base --base-url)
        #[arg(long)]
        base_url: Option<String>,

        /// API key (overrides base --api-key)
        #[arg(long)]
        api_key: Option<String>,

        /// Output results as JSON (overrides base --json)
        #[arg(long)]
        json: bool,

        /// Override system language detection (zh/en)
        #[arg(long)]
        lang: Option<String>,

        /// Save current effective config to file and exit
        #[arg(long, value_hint = ValueHint::FilePath)]
        save_config: Option<PathBuf>,
    },

    /// Interactive chat mode
    Chat {
        /// Initial prompt (use @filepath to read from file)
        #[arg(short, long)]
        prompt: Option<String>,

        /// Max tokens per response
        #[arg(long, default_value = "1024")]
        max_tokens: u32,

        // --- inherited base params (subcommand overrides base) ---

        /// Model to use (overrides base -m)
        #[arg(short, long)]
        model: Option<String>,

        /// API base URL (overrides base --base-url)
        #[arg(long)]
        base_url: Option<String>,

        /// API key (overrides base --api-key)
        #[arg(long)]
        api_key: Option<String>,

        /// Output results as JSON (overrides base --json)
        #[arg(long)]
        json: bool,

        /// Override system language detection (zh/en)
        #[arg(long)]
        lang: Option<String>,

        /// Save current effective config to file and exit
        #[arg(long, value_hint = ValueHint::FilePath)]
        save_config: Option<PathBuf>,
    },
}

fn main() {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).expect("Failed to parse CLI args");

    // Extract subcommand-level overrides (inherit: subcommand > base)
    let (sub_base_url, sub_api_key, sub_model, sub_json, sub_lang, sub_save_config) = match &cli.command {
        Some(Commands::Test { base_url, api_key, model, json, lang, save_config, .. }) => {
            (base_url.clone(), api_key.clone(), model.clone(), *json, lang.clone(), save_config.clone())
        }
        Some(Commands::Chat { base_url, api_key, model, json, lang, save_config, .. }) => {
            (base_url.clone(), api_key.clone(), model.clone(), *json, lang.clone(), save_config.clone())
        }
        None => (None, None, None, false, None, None),
    };

    let effective_base_url = sub_base_url.or(cli.base_url);
    let effective_api_key = sub_api_key.or(cli.api_key);
    let effective_model = sub_model.or(cli.model);
    let effective_json = sub_json || cli.json;
    let effective_lang = sub_lang.or(cli.lang);
    let effective_save_config = sub_save_config.or(cli.save_config);

    let overrides = CliOverrides {
        base_url: effective_base_url.clone(),
        api_key: effective_api_key.clone(),
    };

    let mut config = match Config::resolve(&cli.config, &overrides) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading config: {}", e);
            std::process::exit(1)
        }
    };

    // --lang overrides system detection
    if let Some(lang) = &effective_lang {
        config.lang = lang.clone();
    }

    // Apply model override from CLI
    if let Some(m) = &effective_model {
        config.model = m.clone();
    }

    // Merge config file defaults into subcommand params via value_source
    let sub_matches = matches.subcommand().map(|(_, m)| m);

    let (concurrent, context, max_tokens_test, env_monitor, time_slice) =
        if let Some(Commands::Test { concurrent, context, max_tokens, env_monitor, time_slice, .. }) = &cli.command {
            let tc = config.test.as_ref();
            let sm = sub_matches.unwrap();
            let c = if sm.value_source("concurrent") == Some(clap::parser::ValueSource::DefaultValue) {
                tc.and_then(|t| t.concurrent).unwrap_or(*concurrent)
            } else { *concurrent };
            let ctx = if sm.value_source("context") == Some(clap::parser::ValueSource::DefaultValue) {
                tc.and_then(|t| t.context.clone()).unwrap_or_else(|| context.clone())
            } else { context.clone() };
            let mt = if sm.value_source("max_tokens") == Some(clap::parser::ValueSource::DefaultValue) {
                tc.and_then(|t| t.max_tokens).unwrap_or(*max_tokens)
            } else { *max_tokens };
            let em = if sm.value_source("env_monitor") == Some(clap::parser::ValueSource::DefaultValue) {
                tc.and_then(|t| t.env_monitor).unwrap_or(*env_monitor)
            } else { *env_monitor };
            let ts = if time_slice.is_none() {
                tc.and_then(|t| t.time_slice)
            } else { *time_slice };
            (c, ctx, mt, em, ts)
        } else {
            // No subcommand: restore test params from config file
            let tc = config.test.as_ref();
            let c = tc.and_then(|t| t.concurrent).unwrap_or(1);
            let ctx = tc.and_then(|t| t.context.clone()).unwrap_or_else(|| "1024".to_string());
            let mt = tc.and_then(|t| t.max_tokens).unwrap_or(256);
            let em = tc.and_then(|t| t.env_monitor).unwrap_or(false);
            let ts = tc.and_then(|t| t.time_slice);
            (c, ctx, mt, em, ts)
        };

    let chat_max_tokens = if let Some(Commands::Chat { max_tokens, .. }) = &cli.command {
        let sm = sub_matches.unwrap();
        if sm.value_source("max_tokens") == Some(clap::parser::ValueSource::DefaultValue) {
            config.chat.as_ref().and_then(|c| c.max_tokens).unwrap_or(*max_tokens)
        } else { *max_tokens }
    } else {
        // No subcommand: restore chat params from config file
        config.chat.as_ref().and_then(|c| c.max_tokens).unwrap_or(1024)
    };

    let chat_prompt = if let Some(Commands::Chat { prompt, .. }) = &cli.command {
        prompt.clone()
    } else {
        // No subcommand: restore chat prompt from config file
        config.chat.as_ref().and_then(|c| c.prompt.clone())
    };

    // --save-config: serialize current effective config and exit
    if let Some(save_path) = &effective_save_config {
        match &cli.command {
            Some(Commands::Test { .. }) => {
                config.test = Some(TestConfig {
                    concurrent: Some(concurrent),
                    context: Some(context.clone()),
                    max_tokens: Some(max_tokens_test),
                    env_monitor: Some(env_monitor),
                    time_slice,
                });
            }
            Some(Commands::Chat { prompt, .. }) => {
                config.chat = Some(ChatConfig {
                    max_tokens: Some(chat_max_tokens),
                    prompt: prompt.clone(),
                });
            }
            None => {}
        }
        if let Err(e) = Config::write_config(save_path, &config) {
            eprintln!("Error saving config: {}", e);
            std::process::exit(1);
        }
        println!("Config saved to: {}", save_path.display());
        std::process::exit(0);
    }

    match &cli.command {
        Some(Commands::Test { .. }) => {
            let ts = time_slice.unwrap_or(config.time_slice_interval);
            run_tests(config, concurrent, context, max_tokens_test, env_monitor, ts, effective_json);
        }
        Some(Commands::Chat { prompt, .. }) => {
            if effective_json {
                eprintln!("Warning: --json is not supported in chat mode, ignoring");
            }
            let prompt_text = prompt.as_ref().map(|p| resolve_prompt(p));
            chat::run_chat(config, None, prompt_text, chat_max_tokens);
        }
        None => {
            // No subcommand: determine mode based on config file
            let has_chat = config.chat.is_some();
            let has_test = config.test.is_some();

            if has_chat && !has_test {
                // chat mode from config
                let prompt_text = chat_prompt.as_ref().map(|p| resolve_prompt(p));
                chat::run_chat(config, None, prompt_text, chat_max_tokens);
            } else {
                // default to test mode (includes case where both exist or neither)
                let ts = time_slice.unwrap_or(config.time_slice_interval);
                run_tests(config, concurrent, context, max_tokens_test, env_monitor, ts, effective_json);
            }
        }
    }
}

/// Resolve prompt: if starts with @, read from file; otherwise use as-is
fn resolve_prompt(prompt: &str) -> String {
    if let Some(path) = prompt.strip_prefix('@') {
        match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => {
                eprintln!("Failed to read prompt file '{}': {}", path, e);
                std::process::exit(1);
            }
        }
    } else {
        prompt.to_string()
    }
}

fn run_tests(
    config: Config,
    concurrent: usize,
    context: String,
    max_tokens: u32,
    env_monitor: bool,
    time_slice_interval: f64,
    json_output: bool,
) {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_notify = Arc::new(Notify::new());
    let lang = config.lang.clone();
    let model = config.model.clone();

    // Setup Ctrl+C handler
    {
        let stop_flag = stop_flag.clone();
        let stop_notify = stop_notify.clone();
        ctrlc::set_handler(move || {
            stop_flag.store(true, Ordering::Relaxed);
            stop_notify.notify_waiters();
        })
        .expect("Error setting Ctrl+C handler");
    }

    // Parse context sizes
    let context_sizes = test_runner::parse_step_format(&context);

    if !json_output {
        let (lbl_running, lbl_concurrent, lbl_context, lbl_max_tokens, lbl_model, lbl_slice) = if lang == "zh" {
            ("运行测试", "并发", "上下文大小", "最大Token", "模型", "采样间隔")
        } else {
            ("Running Tests", "Concurrent", "Context sizes", "Max tokens", "Model", "Slice interval")
        };

        println!("\n=== {} ===", lbl_running);
        println!("{}: {}", lbl_concurrent, concurrent);
        println!("{}: {:?}", lbl_context, context_sizes);
        println!("{}: {}", lbl_max_tokens, max_tokens);
        println!("{}: {}", lbl_model, model);
        println!("{}: {}s\n", lbl_slice, time_slice_interval);
    }

    // Create API client
    let base_url = config.base_url.as_ref()
        .expect("API base URL is required (set via --base-url, config file, or environment variable)");
    let api_key = config.api_key.as_ref()
        .expect("API key is required (set via --api-key, config file, or environment variable)");
    let client = ApiClient::new(base_url.clone(), api_key.clone());

    // Run tests with live display (suppressed in JSON mode)
    let runtime = tokio::runtime::Runtime::new().expect("Failed to create runtime");
    let results = runtime.block_on(test_runner::run_live_test(
        client,
        concurrent,
        context_sizes,
        max_tokens,
        model.clone(),
        stop_flag,
        stop_notify,
        time_slice_interval,
        &lang,
        json_output,
    ));

    if json_output {
        // Output structured JSON
        let json = formatter::build_json_results(&results, &model, concurrent, max_tokens, env_monitor, &lang);
        println!("{}", json);
    } else {
        // Print final aggregate results
        formatter::print_final_results(&results, &lang);

        // Print environment info at the end if requested
        if env_monitor {
            let lbl_env = if lang == "zh" { "环境信息" } else { "Environment Information" };
            println!("\n=== {} ===", lbl_env);
            println!("{}", EnvMonitor::collect_with_lang(&lang));
        }
    }
}
