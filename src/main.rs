mod client;
mod config;
mod env_monitor;
mod formatter;
mod test_runner;
mod chat;
mod live_display;
mod utils;

use clap::{Parser, Subcommand, ValueHint};
use client::ApiClient;
use config::{Config, CliOverrides};
use env_monitor::EnvMonitor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "llm-api-tester")]
#[command(about = "OpenAI API Testing CLI Tool", long_about = None)]
struct Cli {
    /// Configuration file path
    #[arg(long, value_hint = ValueHint::FilePath, default_value = "config.yaml")]
    config: PathBuf,

    /// API base URL (overrides config and env var)
    #[arg(long, env = "OPENAI_BASE_URL")]
    base_url: Option<String>,

    /// API key (overrides config and env var)
    #[arg(long, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

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

        /// Model to test
        #[arg(short, long)]
        model: Option<String>,

        /// Enable environment monitoring
        #[arg(short, long)]
        env_monitor: bool,

        /// Time slice interval in seconds for real-time display
        #[arg(long)]
        time_slice: Option<f64>,
    },

    /// Interactive chat mode
    Chat {
        /// Model to use
        #[arg(short, long)]
        model: Option<String>,

        /// Initial prompt (use @filepath to read from file)
        #[arg(short, long)]
        prompt: Option<String>,

        /// Max tokens per response
        #[arg(long, default_value = "1024")]
        max_tokens: u32,
    },
}

fn main() {
    let cli = Cli::parse();

    let overrides = CliOverrides {
        base_url: cli.base_url,
        api_key: cli.api_key,
    };

    let config = match Config::resolve(&cli.config, &overrides) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading config: {}", e);
            std::process::exit(1);
        }
    };

    match &cli.command {
        Some(Commands::Test { concurrent, context, max_tokens, model, env_monitor, time_slice }) => {
            let ts = time_slice.unwrap_or(config.time_slice_interval);
            run_tests(config, *concurrent, context.clone(), *max_tokens, model.clone(), *env_monitor, ts);
        }
        Some(Commands::Chat { model, prompt, max_tokens }) => {
            let prompt_text = prompt.as_ref().map(|p| resolve_prompt(p));
            chat::run_chat(config, model.clone(), prompt_text, *max_tokens);
        }
        None => {
            let ts = config.time_slice_interval;
            run_tests(config, 1, "1024".to_string(), 256, None, false, ts);
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
    model: Option<String>,
    env_monitor: bool,
    time_slice_interval: f64,
) {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let lang = config.lang.clone();

    // Setup Ctrl+C handler
    {
        let stop_flag = stop_flag.clone();
        ctrlc::set_handler(move || {
            stop_flag.store(true, Ordering::Relaxed);
        })
        .expect("Error setting Ctrl+C handler");
    }

    // Parse context sizes
    let context_sizes = test_runner::parse_step_format(&context);

    let (lbl_running, lbl_concurrent, lbl_context, lbl_max_tokens, lbl_model, lbl_slice) = if lang == "zh" {
        ("运行测试", "并发", "上下文大小", "最大Token", "模型", "采样间隔")
    } else {
        ("Running Tests", "Concurrent", "Context sizes", "Max tokens", "Model", "Slice interval")
    };

    // Determine model
    let model = model.unwrap_or(config.default_model.clone());

    println!("\n=== {} ===", lbl_running);
    println!("{}: {}", lbl_concurrent, concurrent);
    println!("{}: {:?}", lbl_context, context_sizes);
    println!("{}: {}", lbl_max_tokens, max_tokens);
    println!("{}: {}", lbl_model, model);
    println!("{}: {}s\n", lbl_slice, time_slice_interval);

    // Create API client
    let client = ApiClient::new(config.base_url.clone(), config.api_key.clone());

    // Run tests with live display
    let runtime = tokio::runtime::Runtime::new().expect("Failed to create runtime");
    let results = runtime.block_on(test_runner::run_live_test(
        client,
        concurrent,
        context_sizes,
        max_tokens,
        model,
        stop_flag,
        time_slice_interval,
        &lang,
    ));

    // Print final aggregate results
    formatter::print_final_results(&results, &lang);

    // Print environment info at the end if requested
    if env_monitor {
        let lbl_env = if lang == "zh" { "环境信息" } else { "Environment Information" };
        println!("\n=== {} ===", lbl_env);
        println!("{}", EnvMonitor::collect_with_lang(&lang));
    }
}
