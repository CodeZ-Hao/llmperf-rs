use crate::client::{ApiClient, count_tokens};
use crate::client::TokenEvent;
use crate::live_display::{LiveDisplay, LiveTestResult};
use rand::Rng;
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, Mutex, Notify};

/// Run tests with live display and event-based streaming
pub async fn run_live_test(
    client: ApiClient,
    concurrent: usize,
    context_sizes: Vec<u32>,
    max_tokens: u32,
    model: String,
    custom_prompt: Option<String>,
    stop_flag: Arc<AtomicBool>,
    stop_notify: Arc<Notify>,
    time_slice_secs: f64,
    lang: &str,
    silent: bool,
) -> Vec<LiveTestResult> {
    let total_requests = context_sizes.len() * concurrent;
    let (tx, mut rx) = mpsc::unbounded_channel::<TokenEvent>();

    let mut display = LiveDisplay::new(total_requests, time_slice_secs, lang, silent);

    // Worker pool: `concurrent` workers pull request specs from a FIFO queue.
    // This makes -j the global concurrency cap AND guarantees strict execution
    // order (requests run in submission order: context-major).
    let (req_tx, req_rx) = mpsc::unbounded_channel::<(usize, u32, Option<String>)>();
    let req_rx = Arc::new(Mutex::new(req_rx));

    for _ in 0..concurrent {
        let client = client.clone();
        let model = model.clone();
        let max_tokens = max_tokens;
        let tx = tx.clone();
        let sn = stop_notify.clone();
        let req_rx = req_rx.clone();
        let stop_flag = stop_flag.clone();

        tokio::spawn(async move {
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let spec = req_rx.lock().await.recv().await;
                let (rid, ctx, task_prompt) = match spec {
                    Some(s) => s,
                    None => break, // queue closed, no more requests
                };
                let prompt = build_test_prompt(ctx, task_prompt.as_deref());
                client.test_streaming_with_events(rid, &model, &prompt, max_tokens, ctx, ctx, tx.clone(), sn.clone()).await;
            }
        });
    }

    // Enqueue all requests in order (context-major: all requests of one context
    // size first, so with -j 1 the test runs 1024 -> 2048 -> ... sequentially)
    let mut request_id = 0usize;
    for context_size in &context_sizes {
        for _ in 0..concurrent {
            let _ = req_tx.send((request_id, *context_size, custom_prompt.clone()));
            request_id += 1;
        }
    }
    drop(req_tx);

    // Drop the original sender so rx closes when all workers finish
    drop(tx);

    // Event loop: process events and tick display
    let tick_interval = std::time::Duration::from_millis(200);
    loop {
        // Check stop_flag every iteration
        if stop_flag.load(Ordering::Relaxed) {
            // Drain remaining buffered events before exiting
            while let Ok(event) = rx.try_recv() {
                display.process_event(event);
            }
            // Give tasks a moment to send their Completed events
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            while let Ok(event) = rx.try_recv() {
                display.process_event(event);
            }
            break;
        }

        match tokio::time::timeout(tick_interval, rx.recv()).await {
            Ok(Some(event)) => {
                display.process_event(event);
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

    // Phase 1: fast assembly using pre-computed per-word token costs.
    // Build the string directly, no intermediate Vec needed.
    let avg_word_len: usize = 5; // " word" ~5 bytes average
    let mut result = String::with_capacity(target * avg_word_len);
    let mut estimated_tokens: usize = 0;

    while estimated_tokens < target {
        let (word, cost) = words[rng.gen_range(0..words.len())];
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(word);
        estimated_tokens += cost;
    }

    // Phase 2: verify accuracy on a small tail segment via tiktoken.
    // The per-word costs are measured with leading space, so cumulative drift
    // is small. We only need to check the last ~200 tokens worth of text
    // to measure the actual drift and compensate.
    let check_size = 200.min(target);
    // Find the word boundary ~check_size words from the end
    let tail_start = {
        let mut spaces = 0;
        let mut pos = result.len();
        for (i, b) in result.bytes().rev().enumerate() {
            if b == b' ' {
                spaces += 1;
                if spaces >= check_size {
                    pos = result.len() - i;
                    break;
                }
            }
        }
        pos
    };

    let tail = &result[tail_start..];
    let tail_estimated: usize = tail.split_whitespace()
        .map(|w| {
            words.iter()
                .find(|(word, _)| *word == w)
                .map(|(_, c)| *c)
                .unwrap_or(1)
        })
        .sum();
    let tail_actual = count_tokens(tail);

    // Extrapolate drift to full string
    if tail_estimated > 0 {
        let drift_ratio = tail_actual as f64 / tail_estimated as f64;
        let corrected_total = (estimated_tokens as f64 * drift_ratio) as usize;

        if corrected_total > target {
            // Trim excess words from the end
            let excess = corrected_total - target;
            for _ in 0..excess {
                if let Some(pos) = result.rfind(' ') {
                    result.truncate(pos);
                } else {
                    break;
                }
            }
        } else if corrected_total < target {
            // Append more words
            let deficit = target - corrected_total;
            for _ in 0..deficit {
                let (word, _) = words[rng.gen_range(0..words.len())];
                result.push(' ');
                result.push_str(word);
            }
        }
    }

    result
}

/// Default task prompt: asks the model to repeat the text above, keeping
/// output length comparable across runs. Used when no custom prompt is given.
const DEFAULT_REPEAT_PROMPT: &str = "Please repeat the text above exactly, without adding anything else.";

/// Random nonce so every request starts with unique tokens, defeating
/// server-side prefix caching (KV cache reuse across requests).
fn generate_random_nonce() -> String {
    let nonce: u64 = rand::thread_rng().gen();
    format!("[nonce:{:016x}]", nonce)
}

/// Build the full test prompt: random nonce + noise text + task prompt.
/// - nonce: breaks prefix caching
/// - noise: fills up to ~`context_size` tokens; its budget accounts for the
///   fixed overhead (nonce + separators) and the task prompt, so the total
///   prompt length stays close to the requested context size
/// - task prompt: the user-provided prompt (-p), or the default repeat prompt
pub fn build_test_prompt(context_size: u32, custom_prompt: Option<&str>) -> String {
    let task_prompt = custom_prompt
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_REPEAT_PROMPT);

    let nonce = generate_random_nonce();

    // Fixed overhead: nonce + newlines + task prompt (local estimate)
    let fixed_overhead = count_tokens(&format!("{}\n\n", nonce))
        + count_tokens(&format!("\n\n{}", task_prompt));
    let noise_tokens = context_size.saturating_sub(fixed_overhead as u32).max(1);

    let noise = generate_random_prompt(noise_tokens);
    format!("{}\n\n{}\n\n{}", nonce, noise, task_prompt)
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
    fn test_build_test_prompt_token_count() {
        for size in [1024, 4096, 16384] {
            let prompt = build_test_prompt(size, None);
            let actual = count_tokens(&prompt) as u32;
            let drift = (actual as i64 - size as i64).abs();
            let tolerance = ((size as f64 * 0.03) as i64).max(10);
            assert!(drift <= tolerance,
                "expected ~{} tokens, got {} (drift {}, tolerance {})", size, actual, drift, tolerance);
        }
    }

    #[test]
    fn test_build_test_prompt_custom() {
        let prompt = build_test_prompt(512, Some("Answer with a single word."));
        assert!(prompt.starts_with("[nonce:"));
        assert!(prompt.ends_with("Answer with a single word."));
        // Nonce must differ between requests (breaks prefix caching)
        let prompt2 = build_test_prompt(512, Some("Answer with a single word."));
        assert_ne!(prompt, prompt2, "nonce should differ between requests");
    }

    #[test]
    fn test_build_test_prompt_tiny_context() {
        // Context smaller than fixed overhead: still produces a valid prompt
        let prompt = build_test_prompt(1, None);
        assert!(prompt.starts_with("[nonce:"));
        assert!(prompt.ends_with(DEFAULT_REPEAT_PROMPT));
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
