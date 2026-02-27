use crate::client::{ApiClient, count_tokens};
use crate::client::TokenEvent;
use crate::live_display::{LiveDisplay, LiveTestResult};
use rand::Rng;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

/// Run tests with live display and event-based streaming
pub async fn run_live_test(
    client: ApiClient,
    concurrent: usize,
    context_sizes: Vec<u32>,
    max_tokens: u32,
    model: String,
    stop_flag: Arc<AtomicBool>,
    time_slice_secs: f64,
    lang: &str,
) -> Vec<LiveTestResult> {
    let total_requests = context_sizes.len() * concurrent;
    let (tx, mut rx) = mpsc::unbounded_channel::<TokenEvent>();

    let mut display = LiveDisplay::new(total_requests, time_slice_secs, lang);

    // Spawn all request tasks
    let mut request_id = 0usize;
    for context_size in &context_sizes {
        for _ in 0..concurrent {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }
            let client = client.clone();
            let model = model.clone();
            let max_tokens = max_tokens;
            let ctx = *context_size;
            let tx = tx.clone();
            let rid = request_id;

            tokio::spawn(async move {
                let prompt = generate_random_prompt(ctx);
                client.test_streaming_with_events(rid, &model, &prompt, max_tokens, tx).await;
            });

            request_id += 1;
        }
    }

    // Drop the original sender so rx closes when all tasks finish
    drop(tx);

    // Event loop: process events and tick display
    let tick_interval = std::time::Duration::from_millis(200);
    loop {
        // Try to receive events with a timeout for ticking
        match tokio::time::timeout(tick_interval, rx.recv()).await {
            Ok(Some(event)) => {
                display.process_event(event);
                // Drain any buffered events
                while let Ok(event) = rx.try_recv() {
                    display.process_event(event);
                }
                display.tick();
            }
            Ok(None) => {
                // Channel closed, all requests done
                display.tick();
                break;
            }
            Err(_) => {
                // Timeout - just tick the display
                display.tick();
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }

    // Final render preserving last state
    display.final_render();
    display.collect_results()
}

/// Pre-computed word token costs (word with leading space, as tiktoken sees it).
/// Computed once via OnceLock to avoid repeated tiktoken calls.
static WORD_TOKENS: OnceLock<Vec<(&'static str, usize)>> = OnceLock::new();

const WORD_POOL: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "the", "is", "at", "we", "be", "to", "of", "in", "it", "on", "that", "this",
    "a", "an", "or", "and", "but", "for", "not", "with", "as", "can", "will",
    "have", "has", "had", "were", "was", "are", "been", "being", "do", "does",
    "did", "made", "from", "which", "their", "they", "them", "than", "then",
];

fn get_word_tokens() -> &'static Vec<(&'static str, usize)> {
    WORD_TOKENS.get_or_init(|| {
        WORD_POOL.iter().map(|w| {
            // Measure token cost of " word" (with leading space, as it appears in context)
            let cost = count_tokens(&format!(" {}", w));
            let cost = cost.max(1);
            (*w, cost)
        }).collect()
    })
}

fn generate_random_prompt(target_tokens: u32) -> String {
    let words = get_word_tokens();
    let mut rng = rand::thread_rng();
    let target = target_tokens as usize;

    // Estimate average bytes per token for pre-allocation
    let avg_word_len: usize = 4; // " xx " ~4 bytes per word
    let mut result = String::with_capacity(target * avg_word_len);
    let mut token_count: usize = 0;

    // First word has no leading space — its token cost may differ slightly,
    // but for these short words the difference is negligible.
    while token_count < target {
        let (word, cost) = words[rng.gen_range(0..words.len())];
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(word);
        token_count += cost;
    }

    result
}

pub fn parse_step_format(input: &str) -> Vec<u32> {
    if input.contains(':') {
        let parts: Vec<&str> = input.split(':').collect();
        if parts.len() == 3 {
            let start: u32 = parts[0].parse().unwrap_or(1024);
            let step: u32 = parts[1].parse().unwrap_or(1024);
            let end: u32 = parts[2].parse().unwrap_or(16384);

            let mut values = Vec::new();
            let mut current = start;
            while current <= end {
                values.push(current);
                current += step;
            }
            values
        } else {
            vec![input.parse().unwrap_or(1024)]
        }
    } else {
        vec![input.parse().unwrap_or(1024)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_step_format_single_value() {
        assert_eq!(parse_step_format("1024"), vec![1024]);
        assert_eq!(parse_step_format("2048"), vec![2048]);
    }

    #[test]
    fn test_parse_step_format_range() {
        assert_eq!(parse_step_format("1024:1024:4096"), vec![1024, 2048, 3072, 4096]);
        assert_eq!(parse_step_format("512:512:1536"), vec![512, 1024, 1536]);
    }

    #[test]
    fn test_parse_step_format_invalid() {
        assert_eq!(parse_step_format("abc"), vec![1024]);
        assert_eq!(parse_step_format("1024:abc"), vec![1024]);
    }

    #[test]
    fn test_parse_step_format_end_not_aligned() {
        // 1024:1000:3000 -> 1024, 2024
        let result = parse_step_format("1024:1000:3000");
        assert_eq!(result, vec![1024, 2024]);
    }

    #[test]
    fn test_generate_random_prompt_token_count() {
        let prompt = generate_random_prompt(100);
        let actual = count_tokens(&prompt);
        // Should be within a small margin of the target
        assert!(actual >= 95 && actual <= 110,
            "Expected ~100 tokens, got {}", actual);
    }

    #[test]
    fn test_generate_random_prompt_100k_performance() {
        let start = std::time::Instant::now();
        let prompt = generate_random_prompt(100_000);
        let elapsed = start.elapsed();

        let actual = count_tokens(&prompt);
        println!("100K prompt: {} tokens generated in {:?}", actual, elapsed);

        assert!(elapsed.as_secs_f64() < 1.0,
            "100K prompt generation took {:?}, must be under 1s", elapsed);
    }
}
