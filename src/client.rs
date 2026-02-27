use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::OnceLock;
use tokio::sync::{mpsc, Notify};

/// Events emitted during streaming for real-time tracking
#[derive(Debug, Clone)]
pub enum TokenEvent {
    /// Request has been sent, waiting for first token
    RequestStarted {
        request_id: usize,
        start_time: Instant,
        prompt_tokens: u32,
    },
    /// First token received - marks end of prefill
    FirstToken {
        request_id: usize,
        time: Instant,
    },
    /// A chunk of output tokens received
    TokensReceived {
        request_id: usize,
        #[allow(dead_code)]
        time: Instant,
        token_count: u32,
    },
    /// Request completed
    Completed {
        request_id: usize,
        time: Instant,
        completion_tokens: u32,
        prompt_tokens: u32,
        success: bool,
        error: Option<String>,
    },
}

/// Global tokenizer for token counting
static TOKENIZER: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();

/// Get or initialize the tokenizer
fn get_tokenizer() -> Option<&'static tiktoken_rs::CoreBPE> {
    TOKENIZER.get_or_init(|| {
        tiktoken_rs::cl100k_base().ok()
    }).as_ref()
}

/// Count tokens using tiktoken, fallback to character estimation
pub fn count_tokens(text: &str) -> usize {
    if let Some(tkn) = get_tokenizer() {
        let tokens = tkn.encode_with_special_tokens(text);
        tokens.len()
    } else {
        estimate_tokens(text)
    }
}

/// Estimate tokens based on character count (rough approximation)
fn estimate_tokens(text: &str) -> usize {
    // Average token is about 4 characters in English, but can be different for Chinese
    // This is a rough estimation
    text.chars()
        .map(|c| if c.is_ascii() { 1 } else { 2 })  // Chinese chars count as 2
        .sum::<usize>() / 4
}

#[derive(Debug, Serialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
    #[serde(default)]
    stream_options: Option<StreamOptions>,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatResponse {
    choices: Option<Vec<Choice>>,
    #[serde(default)]
    error: Option<ApiError>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Choice {
    message: Option<ResponseMessage>,
    #[serde(default)]
    delta: Option<DeltaMessage>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResponseMessage {
    content: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize, Clone)]
struct DeltaMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub(crate) struct Usage {
    #[serde(rename = "prompt_tokens", default)]
    prompt_tokens: Option<u32>,
    #[serde(rename = "completion_tokens", default)]
    completion_tokens: Option<u32>,
    #[serde(rename = "total_tokens", default)]
    total_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ApiError {
    message: Option<String>,
    #[serde(default)]
    error: Option<InnerError>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InnerError {
    message: Option<String>,
}

/// Parsed SSE delta event
pub struct SseDelta {
    pub content: Option<String>,
    pub(crate) usage: Option<Usage>,
}

/// Shared SSE line parser — extracts delta content and usage from a single SSE line
fn parse_sse_line(line: &str) -> Option<SseDelta> {
    let line = line.trim();
    if !line.starts_with("data:") {
        return None;
    }
    let data = line.trim_start_matches("data:").trim();
    if data.is_empty() {
        return None;
    }
    if data == "[DONE]" {
        return None;
    }
    let resp: ChatResponse = serde_json::from_str(data).ok()?;
    let usage = resp.usage;
    let content = resp.choices
        .and_then(|c| c.first()?.delta.clone())
        .and_then(|d| d.content.or(d.reasoning_content));
    Some(SseDelta { content, usage })
}

/// Process buffered bytes into SSE deltas, calling handler for each
fn process_sse_buffer<F>(buffer: &mut String, bytes: &[u8], mut handler: F)
where
    F: FnMut(SseDelta),
{
    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
        buffer.push_str(&text);
        while let Some(pos) = buffer.find('\n') {
            let line = buffer.drain(..pos + 1).collect::<String>();
            if let Some(delta) = parse_sse_line(&line) {
                handler(delta);
            }
        }
    }
}

/// Async helper: waits for stop notification (zero-latency wakeup)
async fn wait_stop(notify: &Notify) {
    notify.notified().await;
}

#[derive(Clone)]
pub struct ApiClient {
    pub(crate) client: Client,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
}

/// Result from streaming chat
#[derive(Debug, Clone)]
pub struct ChatStreamResult {
    pub content: String,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub prefill_tps: Option<f64>,  // tokens per second for input
    pub decode_tps: Option<f64>,  // tokens per second for output
}

impl ApiClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            base_url,
            api_key,
        }
    }

    /// Event-based streaming for real-time display
    pub async fn test_streaming_with_events(
        &self,
        request_id: usize,
        model: &str,
        prompt: &str,
        max_tokens: u32,
        prompt_tokens: u32,
        tx: mpsc::UnboundedSender<TokenEvent>,
        stop_notify: Arc<Notify>,
    ) {
        let start = Instant::now();

        if tx.send(TokenEvent::RequestStarted {
            request_id,
            start_time: start,
            prompt_tokens,
        }).is_err() {
            eprintln!("[req {}] event channel closed before RequestStarted", request_id);
            return;
        }

        let request = ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
            max_tokens,
            temperature: 0.7,
            stream: true,
            stream_options: Some(StreamOptions { include_usage: true }),
        };

        let url = format!("{}/chat/completions", self.base_url);

        let response = match self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let _ = tx.send(TokenEvent::Completed {
                    request_id,
                    time: Instant::now(),
                    completion_tokens: 0,
                    prompt_tokens,
                    success: false,
                    error: Some(format!("Request failed: {}", e)),
                });
                return;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            let _ = tx.send(TokenEvent::Completed {
                request_id,
                time: Instant::now(),
                completion_tokens: 0,
                prompt_tokens,
                success: false,
                error: Some(format!("HTTP {}: {}", status, error_text)),
            });
            return;
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut first_token_sent = false;
        let mut output_token_count: u32 = 0;
        let mut server_completion_tokens: Option<u32> = None;

        loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = wait_stop(&stop_notify) => {
                    let final_tokens = server_completion_tokens.unwrap_or(output_token_count);
                    let _ = tx.send(TokenEvent::Completed {
                        request_id,
                        time: Instant::now(),
                        completion_tokens: final_tokens,
                        prompt_tokens,
                        success: false,
                        error: Some("interrupted".to_string()),
                    });
                    return;
                }
            };

            match chunk {
                Some(Ok(bytes)) => {
                    let now = Instant::now();
                    process_sse_buffer(&mut buffer, &bytes, |delta| {
                        if let Some(usage) = delta.usage {
                            server_completion_tokens = usage.completion_tokens;
                        }
                        if let Some(content) = delta.content {
                            if !content.is_empty() {
                                if !first_token_sent {
                                    first_token_sent = true;
                                    if tx.send(TokenEvent::FirstToken {
                                        request_id,
                                        time: now,
                                    }).is_err() {
                                        eprintln!("[req {}] event channel closed during FirstToken", request_id);
                                    }
                                }
                                let chunk_tokens = count_tokens(&content) as u32;
                                let chunk_tokens = chunk_tokens.max(1);
                                output_token_count += chunk_tokens;
                                if tx.send(TokenEvent::TokensReceived {
                                    request_id,
                                    time: now,
                                    token_count: chunk_tokens,
                                }).is_err() {
                                    eprintln!("[req {}] event channel closed during TokensReceived", request_id);
                                }
                            }
                        }
                    });
                }
                Some(Err(e)) => {
                    let _ = tx.send(TokenEvent::Completed {
                        request_id,
                        time: Instant::now(),
                        completion_tokens: output_token_count,
                        prompt_tokens,
                        success: false,
                        error: Some(format!("Stream error: {}", e)),
                    });
                    return;
                }
                None => {
                    // Stream ended normally
                    break;
                }
            }
        }

        let final_tokens = server_completion_tokens.unwrap_or(output_token_count);
        let _ = tx.send(TokenEvent::Completed {
            request_id,
            time: Instant::now(),
            completion_tokens: final_tokens,
            prompt_tokens,
            success: true,
            error: None,
        });
    }

    /// Streaming chat for interactive mode
    pub async fn chat_streaming<F>(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
        mut on_chunk: F,
    ) -> Result<ChatStreamResult, String>
    where
        F: FnMut(&str),
    {
        // Compute prompt tokens via tiktoken for accuracy (chat context is typically small).
        // Will be replaced by server-provided value if available.
        let prompt_text: String = messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let local_prompt_tokens = count_tokens(&prompt_text) as u32;

        let request = ChatRequest {
            model: model.to_string(),
            messages,
            max_tokens,
            temperature: 0.7,
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
        };

        let url = format!("{}/chat/completions", self.base_url);

        let start = std::time::Instant::now();

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("HTTP error: {} - {}", status, error_text));
        }

        // Read body stream chunk by chunk to get timing info
        let mut stream = response.bytes_stream();
        let mut full_content = String::new();
        let mut first_token_time: Option<std::time::Duration> = None;
        let mut last_token_time: Option<std::time::Duration> = None;
        let mut buffer = String::new();
        let mut server_completion_tokens: Option<u32> = None;
        let mut server_prompt_tokens: Option<u32> = None;

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| e.to_string())?;
            let now = start.elapsed();

            process_sse_buffer(&mut buffer, &bytes, |delta| {
                if let Some(usage) = delta.usage {
                    if let Some(ct) = usage.completion_tokens {
                        server_completion_tokens = Some(ct);
                    }
                    if let Some(pt) = usage.prompt_tokens {
                        server_prompt_tokens = Some(pt);
                    }
                }
                if let Some(content) = delta.content {
                    if first_token_time.is_none() {
                        first_token_time = Some(now);
                    }
                    last_token_time = Some(now);
                    on_chunk(&content);
                    full_content.push_str(&content);
                }
            });
        }

        // Use server-provided tokens if available, otherwise estimate
        let prompt_tokens = server_prompt_tokens
            .unwrap_or(local_prompt_tokens);
        let output_tokens = server_completion_tokens
            .unwrap_or_else(|| (full_content.len() / 4) as u32);

        // Calculate prefill speed
        let prefill_tps = first_token_time.map(|d| {
            let seconds = d.as_secs_f64();
            if seconds > 0.0 && prompt_tokens > 0 {
                prompt_tokens as f64 / seconds
            } else {
                0.0
            }
        });

        let decode_tps = if let (Some(first), Some(last)) = (first_token_time, last_token_time) {
            let decode_time = last.as_secs_f64() - first.as_secs_f64();
            if decode_time > 0.001 && output_tokens > 0 {
                Some(output_tokens as f64 / decode_time)
            } else {
                None
            }
        } else {
            None
        };

        Ok(ChatStreamResult {
            content: full_content,
            prompt_tokens: Some(prompt_tokens),
            completion_tokens: Some(output_tokens),
            prefill_tps,
            decode_tps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_tokens_english() {
        let count = count_tokens("hello world");
        assert!(count > 0 && count <= 3, "Expected 2 tokens, got {}", count);
    }

    #[test]
    fn test_count_tokens_empty() {
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_ascii() {
        let est = estimate_tokens("hello world");
        assert_eq!(est, 2);
    }

    #[test]
    fn test_estimate_tokens_chinese() {
        let est = estimate_tokens("你好吗");
        assert_eq!(est, 1);
    }

    #[test]
    fn test_parse_sse_line_valid() {
        let line = r#"data: {"choices":[{"delta":{"content":"hi"}}]}"#;
        let delta = parse_sse_line(line).unwrap();
        assert_eq!(delta.content.unwrap(), "hi");
    }

    #[test]
    fn test_parse_sse_line_done() {
        assert!(parse_sse_line("data: [DONE]").is_none());
    }

    #[test]
    fn test_parse_sse_line_empty() {
        assert!(parse_sse_line("").is_none());
        assert!(parse_sse_line("event: ping").is_none());
    }

    #[test]
    fn test_process_sse_buffer_multiline() {
        let mut buffer = String::new();
        let input = "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n";
        let mut contents = Vec::new();
        process_sse_buffer(&mut buffer, input.as_bytes(), |delta| {
            if let Some(c) = delta.content {
                contents.push(c);
            }
        });
        assert_eq!(contents, vec!["a", "b"]);
    }
}
