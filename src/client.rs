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
        /// Target context size (for display; may differ from actual server input tokens)
        context_size: u32,
    },
    /// First token received - marks end of prefill
    FirstToken {
        request_id: usize,
        time: Instant,
    },
    /// A chunk of output content received (token counts are derived from the
    /// accumulated text — per-chunk tokenization overestimates on partial tokens)
    TokensReceived {
        request_id: usize,
        #[allow(dead_code)]
        time: Instant,
        content: String,
    },
    /// Request completed
    Completed {
        request_id: usize,
        time: Instant,
        /// Server-reported completion tokens (from the final chunk's usage
        /// block). 0 when the server did not report usage — the display falls
        /// back to counting the accumulated text locally.
        completion_tokens: u32,
        prompt_tokens: u32,
        /// Whether completion_tokens comes from the server's usage block
        /// (authoritative, model-tokenizer count) or is unset (server_usage=false)
        server_usage: bool,
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
    /// Passed to the server's chat template, used to toggle model thinking.
    /// Both common keys are sent so it works across serving stacks:
    /// - `thinking`: DeepSeek V4 style (SGLang, e.g. deepseek-v4-flash)
    /// - `enable_thinking`: Qwen3 style (llama.cpp / vLLM / DashScope)
    chat_template_kwargs: ChatTemplateKwargs,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct ChatTemplateKwargs {
    thinking: bool,
    enable_thinking: bool,
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

/// Process buffered bytes into SSE deltas, calling handler for each.
/// The buffer holds raw bytes: TCP chunks can split a multi-byte UTF-8
/// character, so conversion happens only on complete lines. A line is
/// always valid UTF-8 — '\n' (0x0A) can never appear inside a multi-byte
/// character, so no content is dropped at chunk boundaries.
fn process_sse_buffer<F>(buffer: &mut Vec<u8>, bytes: &[u8], mut handler: F)
where
    F: FnMut(SseDelta),
{
    buffer.extend_from_slice(bytes);
    while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
        let line = buffer.drain(..=pos).collect::<Vec<u8>>();
        let line = String::from_utf8_lossy(&line);
        if let Some(delta) = parse_sse_line(&line) {
            handler(delta);
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
    pub total_duration_secs: f64,
}

impl ApiClient {
    /// `timeout`: None = no limit (reqwest default)
    pub fn new(base_url: String, api_key: String, timeout: Option<Duration>) -> Self {
        let mut builder = Client::builder();
        if let Some(t) = timeout {
            builder = builder.timeout(t);
        }
        let client = builder.build().expect("Failed to build HTTP client");

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
        context_size: u32,
        tx: mpsc::UnboundedSender<TokenEvent>,
        stop_notify: Arc<Notify>,
        enable_thinking: bool,
    ) {
        let start = Instant::now();

        if tx.send(TokenEvent::RequestStarted {
            request_id,
            start_time: start,
            prompt_tokens,
            context_size,
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
            chat_template_kwargs: ChatTemplateKwargs { thinking: enable_thinking, enable_thinking },
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
                    server_usage: false,
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
                server_usage: false,
                success: false,
                error: Some(format!("HTTP {}: {}", status, error_text)),
            });
            return;
        }

        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut first_token_sent = false;
        let mut server_completion_tokens: Option<u32> = None;
        let mut server_prompt_tokens: Option<u32> = None;

        loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = wait_stop(&stop_notify) => {
                    let _ = tx.send(TokenEvent::Completed {
                        request_id,
                        time: Instant::now(),
                        completion_tokens: server_completion_tokens.unwrap_or(0),
                        prompt_tokens: server_prompt_tokens.unwrap_or(prompt_tokens),
                        server_usage: server_completion_tokens.is_some(),
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
                            server_prompt_tokens = usage.prompt_tokens;
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
                                if tx.send(TokenEvent::TokensReceived {
                                    request_id,
                                    time: now,
                                    content,
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
                        completion_tokens: server_completion_tokens.unwrap_or(0),
                        prompt_tokens: server_prompt_tokens.unwrap_or(prompt_tokens),
                        server_usage: server_completion_tokens.is_some(),
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

        // Server-reported usage (real tokenizer count) is authoritative; when the
        // server does not report it (completion_tokens = 0, server_usage = false),
        // the display falls back to counting the accumulated text locally.
        let final_prompt_tokens = server_prompt_tokens.unwrap_or(prompt_tokens);
        let _ = tx.send(TokenEvent::Completed {
            request_id,
            time: Instant::now(),
            completion_tokens: server_completion_tokens.unwrap_or(0),
            prompt_tokens: final_prompt_tokens,
            server_usage: server_completion_tokens.is_some(),
            success: true,
            error: None,
        });
    }

    /// Fetch the first model from GET /models, used as the default model when
    /// none is configured. Returns None if the endpoint is unavailable.
    pub async fn fetch_first_model(&self) -> Option<String> {
        let url = format!("{}/models", self.base_url);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let text = resp.text().await.ok()?;
        #[derive(Deserialize)]
        struct ModelsResponse {
            data: Vec<ModelEntry>,
        }
        #[derive(Deserialize)]
        struct ModelEntry {
            id: String,
        }
        let models: ModelsResponse = serde_json::from_str(&text).ok()?;
        models.data.into_iter().next().map(|m| m.id)
    }

    /// Streaming chat for interactive mode
    pub async fn chat_streaming<F>(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
        enable_thinking: bool,
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
            chat_template_kwargs: ChatTemplateKwargs { thinking: enable_thinking, enable_thinking },
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
        let mut buffer: Vec<u8> = Vec::new();
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

        // Use server-provided tokens if available, otherwise count the full
        // accumulated text once (per-chunk estimates inflate on partial tokens)
        let prompt_tokens = server_prompt_tokens
            .unwrap_or(local_prompt_tokens);
        let output_tokens = server_completion_tokens
            .unwrap_or_else(|| count_tokens(&full_content) as u32);

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
            total_duration_secs: start.elapsed().as_secs_f64(),
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
        let mut buffer: Vec<u8> = Vec::new();
        let input = "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n";
        let mut contents = Vec::new();
        process_sse_buffer(&mut buffer, input.as_bytes(), |delta| {
            if let Some(c) = delta.content {
                contents.push(c);
            }
        });
        assert_eq!(contents, vec!["a", "b"]);
    }

    #[test]
    fn test_process_sse_buffer_split_multibyte() {
        // A multi-byte UTF-8 char (中 = E4 B8 AD) split across TCP chunks must
        // not be dropped: the buffer holds raw bytes until a complete line
        // (terminated by '\n') is available, so the char reassembles correctly.
        let mut buffer: Vec<u8> = Vec::new();
        let prefix = b"data: {\"choices\":[{\"delta\":{\"content\":\"";
        let suffix = b"\"}}]}\n";
        let mut contents = Vec::new();

        // Chunk 1: line up to the first two bytes of 中 (E4 B8), no newline yet
        let mut chunk1 = prefix.to_vec();
        chunk1.extend_from_slice(&[0xE4, 0xB8]);
        process_sse_buffer(&mut buffer, &chunk1, |d| {
            if let Some(c) = d.content {
                contents.push(c);
            }
        });
        assert!(contents.is_empty(), "partial line must not be parsed yet");

        // Chunk 2: the tail byte of 中 (AD) + 好 (E5 A5 BD) + line terminator
        let mut chunk2: Vec<u8> = vec![0xAD, 0xE5, 0xA5, 0xBD];
        chunk2.extend_from_slice(suffix);
        process_sse_buffer(&mut buffer, &chunk2, |d| {
            if let Some(c) = d.content {
                contents.push(c);
            }
        });

        assert_eq!(contents, vec!["中好"]);
    }
}
