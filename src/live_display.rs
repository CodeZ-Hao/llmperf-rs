use crate::client::TokenEvent;
use crate::utils::{display_width, pad_left, pad_center};
use std::io::{self, Write};
use std::time::Instant;

/// State of a single request being tracked
#[derive(Debug, Clone)]
pub struct RequestState {
    pub request_id: usize,
    pub start_time: Option<Instant>,
    pub first_token_time: Option<Instant>,
    pub end_time: Option<Instant>,
    pub prompt_tokens: u32,
    /// Target context size of this request (from -c, for display)
    pub context_size: u32,
    pub completed: bool,
    pub success: bool,
    pub error: Option<String>,
    pub completion_tokens: u32,
    pub slice_tokens: u32,
    pub slice_decode_start: Option<Instant>,
    pub final_decode_tps: Option<f64>,
}

/// Aggregated time-slice bucket
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TimeBucket {
    pub prefill_input_tokens: f64,
    pub prefill_active: usize,
    pub decode_output_tokens: u32,
    pub decode_active: usize,
    pub duration_secs: f64,
}

/// Final result data from live test
#[derive(Debug, Clone)]
pub struct LiveTestResult {
    pub request_id: usize,
    /// Absolute request start time, for wall-clock interval arithmetic
    pub start: Instant,
    /// Whether the first token arrived (prefill completed)
    pub prefilled: bool,
    /// Target context size of this request
    pub context_size: u32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub prefill_duration_secs: f64,
    pub decode_duration_secs: f64,
    pub total_duration_secs: f64,
    pub success: bool,
    pub error: Option<String>,
}

const SPINNER_CHARS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Total duration (seconds) of the union of time intervals: sort by start,
/// merge overlapping spans, sum. Used for system prefill throughput —
/// concurrent prefills share the same wall-clock span and must not stack.
fn union_duration(intervals: &[(Instant, Instant)]) -> f64 {
    if intervals.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<(Instant, Instant)> = intervals.to_vec();
    sorted.sort_unstable_by_key(|(s, _)| *s);
    let mut total = 0.0;
    let mut cur = sorted[0];
    for &(s, e) in &sorted[1..] {
        if s > cur.1 {
            total += cur.1.duration_since(cur.0).as_secs_f64();
            cur = (s, e);
        } else if e > cur.1 {
            cur.1 = e;
        }
    }
    total + cur.1.duration_since(cur.0).as_secs_f64()
}


impl RequestState {
    pub fn new(request_id: usize) -> Self {
        Self {
            request_id,
            start_time: None,
            first_token_time: None,
            end_time: None,
            prompt_tokens: 0,
            context_size: 0,
            completed: false,
            success: false,
            error: None,
            completion_tokens: 0,
            slice_tokens: 0,
            slice_decode_start: None,
            final_decode_tps: None,
        }
    }

    pub fn is_prefill(&self) -> bool {
        self.start_time.is_some() && self.first_token_time.is_none() && !self.completed
    }

    pub fn is_decode(&self) -> bool {
        self.first_token_time.is_some() && !self.completed
    }
}

pub struct LiveDisplay {
    pub requests: Vec<RequestState>,
    pub time_slice_secs: f64,
    pub lang: String,
    pub last_render_lines: usize,
    pub test_start: Instant,
    pub last_slice_time: Instant,
    pub buckets: Vec<TimeBucket>,
    pub spinner_idx: usize,
    pub silent: bool,
}

impl LiveDisplay {
    pub fn new(total_concurrent: usize, time_slice_secs: f64, lang: &str, silent: bool) -> Self {
        let now = Instant::now();
        let mut requests = Vec::with_capacity(total_concurrent);
        for i in 0..total_concurrent {
            requests.push(RequestState::new(i));
        }
        Self {
            requests,
            time_slice_secs,
            lang: lang.to_string(),
            last_render_lines: 0,
            test_start: now,
            last_slice_time: now,
            buckets: Vec::new(),
            spinner_idx: 0,
            silent,
        }
    }

    /// Process a single token event and update internal state
    pub fn process_event(&mut self, event: TokenEvent) {
        match event {
            TokenEvent::RequestStarted { request_id, start_time, prompt_tokens, context_size } => {
                if let Some(req) = self.requests.get_mut(request_id) {
                    req.start_time = Some(start_time);
                    req.prompt_tokens = prompt_tokens;
                    req.context_size = context_size;
                }
            }
            TokenEvent::FirstToken { request_id, time } => {
                if let Some(req) = self.requests.get_mut(request_id) {
                    req.first_token_time = Some(time);
                    // Mark decode start within current slice
                    req.slice_decode_start = Some(time);
                }
            }
            TokenEvent::TokensReceived { request_id, token_count, .. } => {
                if let Some(req) = self.requests.get_mut(request_id) {
                    req.slice_tokens += token_count;
                    req.completion_tokens += token_count;
                }
            }
            TokenEvent::Completed { request_id, completion_tokens, prompt_tokens, success, error, time } => {
                if let Some(req) = self.requests.get_mut(request_id) {
                    req.completed = true;
                    req.success = success;
                    req.error = error;
                    req.end_time = Some(time);
                    if completion_tokens > req.completion_tokens {
                        req.completion_tokens = completion_tokens;
                    }
                    req.prompt_tokens = prompt_tokens;
                    // Calculate final decode tps at completion time
                    if let Some(first) = req.first_token_time {
                        let decode_dur = time.duration_since(first).as_secs_f64();
                        if decode_dur > 0.001 && req.completion_tokens > 0 {
                            req.final_decode_tps = Some(req.completion_tokens as f64 / decode_dur);
                        }
                    }
                }
            }
        }
    }

    /// Called periodically to collect time-slice bucket and render
    pub fn tick(&mut self) {
        let now = Instant::now();
        let slice_duration = now.duration_since(self.last_slice_time).as_secs_f64();

        if slice_duration >= self.time_slice_secs {
            self.collect_bucket(now);
            self.last_slice_time = now;
            self.reset_slice_counters(now);
        }

        self.spinner_idx = (self.spinner_idx + 1) % SPINNER_CHARS.len();
        self.render(now);
    }

    /// Collect a time-slice bucket from current state. Prefill is only
    /// measurable once the first token arrives, so only completed prefills
    /// (first_token_time set) are counted, with their full token counts.
    fn collect_bucket(&mut self, now: Instant) {
        let slice_dur = now.duration_since(self.last_slice_time).as_secs_f64();
        if slice_dur < 0.001 {
            return;
        }

        let mut prefill_input_tokens: f64 = 0.0;
        let mut prefill_active: usize = 0;
        let mut decode_output_tokens: u32 = 0;
        let mut decode_active: usize = 0;

        for req in &self.requests {
            if req.start_time.is_some() && req.first_token_time.is_some() {
                prefill_active += 1;
                prefill_input_tokens += req.prompt_tokens as f64;
            }
            if req.is_decode() || (req.completed && req.slice_tokens > 0) {
                decode_active += 1;
                decode_output_tokens += req.slice_tokens;
            }
        }

        self.buckets.push(TimeBucket {
            prefill_input_tokens,
            prefill_active,
            decode_output_tokens,
            decode_active,
            duration_secs: slice_dur,
        });
    }

    /// Reset per-slice counters after a bucket is collected
    fn reset_slice_counters(&mut self, now: Instant) {
        for req in &mut self.requests {
            req.slice_tokens = 0;
            if req.is_decode() {
                req.slice_decode_start = Some(now);
            } else {
                req.slice_decode_start = None;
            }
        }
    }

    /// Render the live table to terminal
    fn render(&mut self, now: Instant) {
        if self.silent {
            return;
        }
        let mut out = io::stdout();

        // Move cursor up to overwrite previous render
        if self.last_render_lines > 0 {
            write!(out, "\x1b[{}A\r", self.last_render_lines).ok();
        }

        let elapsed = now.duration_since(self.test_start).as_secs_f64();
        let slice_elapsed = now.duration_since(self.last_slice_time).as_secs_f64();

        let lines = self.build_table_lines(elapsed, slice_elapsed, now);
        self.last_render_lines = lines.len();

        for line in &lines {
            // Clear line and write
            write!(out, "\x1b[2K{}\n", line).ok();
        }
        out.flush().ok();
    }

    /// Build the table lines for rendering
    fn build_table_lines(&self, elapsed: f64, slice_elapsed: f64, now: Instant) -> Vec<String> {
        let mut lines = Vec::new();

        // Column widths
        let col_id = 6;
        let col_ctx = 8;
        let col_status = 12;
        let col_prefill = 14;
        let col_decode = 14;
        let col_tokens = 10;
        let col_time = 10;

        let (lbl_id, lbl_ctx, lbl_status, lbl_prefill, lbl_decode, lbl_tokens, lbl_time) =
            if self.lang == "zh" {
                ("#", "上下文", "状态", "Prefill(t/s)", "Decode(t/s)", "输出Token", "耗时(s)")
            } else {
                ("#", "Ctx", "Status", "Prefill(t/s)", "Decode(t/s)", "Out Toks", "Time(s)")
            };

        // Time header
        let time_str = if self.lang == "zh" {
            format!("  运行 {:.1}s", elapsed)
        } else {
            format!("  Elapsed {:.1}s", elapsed)
        };
        lines.push(time_str);

        // Table header
        let header = format!(
            " {} | {} | {} | {} | {} | {} | {}",
            pad_center(lbl_id, col_id),
            pad_center(lbl_ctx, col_ctx),
            pad_center(lbl_status, col_status),
            pad_center(lbl_prefill, col_prefill),
            pad_center(lbl_decode, col_decode),
            pad_center(lbl_tokens, col_tokens),
            pad_center(lbl_time, col_time),
        );
        lines.push(header.clone());
        lines.push("-".repeat(display_width(&header)));

        // Request rows
        for req in &self.requests {
            let id_str = format!("{}", req.request_id + 1);
            let (status_str, prefill_str, decode_str) = self.format_request_metrics(req, slice_elapsed, now);
            let ctx_str = format!("{}", req.context_size);
            let tokens_str = if req.completion_tokens > 0 {
                format!("{}", req.completion_tokens)
            } else {
                "-".to_string()
            };
            let time_str = if let Some(start) = req.start_time {
                let end = req.end_time.unwrap_or(now);
                format!("{:.1}", end.duration_since(start).as_secs_f64())
            } else {
                "-".to_string()
            };

            let row = format!(
                " {} | {} | {} | {} | {} | {} | {}",
                pad_center(&id_str, col_id),
                pad_center(&ctx_str, col_ctx),
                pad_center(&status_str, col_status),
                pad_left(&prefill_str, col_prefill),
                pad_left(&decode_str, col_decode),
                pad_left(&tokens_str, col_tokens),
                pad_left(&time_str, col_time),
            );
            lines.push(row);
        }

        // Separator
        lines.push("-".repeat(display_width(&header)));

        // System-wide aggregate for current slice
        let (sys_prefill, sys_decode) = self.calc_system_throughput(slice_elapsed);
        let sys_line = if self.lang == "zh" {
            format!(
                " 系统吞吐  Prefill: {:.0} input t/s | Decode: {:.1} output t/s",
                sys_prefill, sys_decode
            )
        } else {
            format!(
                " System    Prefill: {:.0} input t/s | Decode: {:.1} output t/s",
                sys_prefill, sys_decode
            )
        };
        lines.push(sys_line);

        lines
    }

    /// Format a single request's status and throughput for display.
    /// Returns (status, prefill tps, decode tps).
    fn format_request_metrics(&self, req: &RequestState, slice_elapsed: f64, now: Instant) -> (String, String, String) {
        // Prefill tps is only measurable once the first token arrives (prefill
        // phase complete, decode started): the duration is unknown before that,
        // so no running estimate is shown. After first token the value is
        // fixed; at completion it settles to the server-reported token count.
        let prefill_str = if let (Some(start), Some(ft)) = (req.start_time, req.first_token_time) {
            let prefill_dur = ft.duration_since(start).as_secs_f64();
            if prefill_dur > 0.001 && req.prompt_tokens > 0 {
                format!("{:.1}", req.prompt_tokens as f64 / prefill_dur)
            } else {
                "-".to_string()
            }
        } else {
            "-".to_string()
        };

        if req.start_time.is_none() {
            let lbl = if self.lang == "zh" { "等待" } else { "Wait" };
            return (lbl.to_string(), "-".to_string(), "-".to_string());
        }

        if req.completed {
            if req.success {
                let lbl = if self.lang == "zh" { "完成" } else { "Done" };
                let tps = match req.final_decode_tps {
                    Some(v) => format!("{:.1}", v),
                    None => "-".to_string(),
                };
                return (lbl.to_string(), prefill_str, tps);
            } else {
                let lbl = if self.lang == "zh" { "失败" } else { "Fail" };
                return (lbl.to_string(), prefill_str, "-".to_string());
            }
        }

        if req.is_prefill() {
            let spinner = SPINNER_CHARS[self.spinner_idx];
            let lbl = if self.lang == "zh" {
                format!("{} Prefill", spinner)
            } else {
                format!("{} Prefill", spinner)
            };
            return (lbl, prefill_str, "-".to_string());
        }

        if req.is_decode() {
            // Calculate decode tps for current slice
            let decode_time_in_slice = if let Some(decode_start) = req.slice_decode_start {
                now.duration_since(decode_start).as_secs_f64()
            } else {
                slice_elapsed
            };

            let tps = if decode_time_in_slice > 0.01 && req.slice_tokens > 0 {
                format!("{:.1}", req.slice_tokens as f64 / decode_time_in_slice)
            } else {
                "-".to_string()
            };

            let lbl = "Decode".to_string();
            return (lbl, prefill_str, tps);
        }

        ("-".to_string(), "-".to_string(), "-".to_string())
    }

    /// Calculate system-wide throughput for the current slice.
    /// Prefill: only completed prefills are measurable — the duration is only
    /// known once the first token arrives. Rate = total tokens of completed
    /// prefills / the union of their prefill intervals (concurrent prefills
    /// sharing wall-clock time don't stack). In-flight prefills contribute
    /// nothing until they complete. Decode: tokens received in the slice /
    /// slice duration.
    fn calc_system_throughput(&self, slice_elapsed: f64) -> (f64, f64) {
        if slice_elapsed < 0.01 {
            return (0.0, 0.0);
        }

        let mut prefill_tokens: f64 = 0.0;
        let mut prefill_intervals: Vec<(Instant, Instant)> = Vec::new();
        for req in &self.requests {
            if let (Some(start), Some(ft)) = (req.start_time, req.first_token_time) {
                prefill_tokens += req.prompt_tokens as f64;
                prefill_intervals.push((start, ft));
            }
        }
        let prefill_span = union_duration(&prefill_intervals);
        let prefill_tps = if prefill_span > 0.001 {
            prefill_tokens / prefill_span
        } else {
            0.0
        };

        // Decode: tokens actually received in this slice
        let mut total_decode_tokens: u32 = 0;
        for req in &self.requests {
            if req.is_decode() || (req.completed && req.slice_tokens > 0) {
                total_decode_tokens += req.slice_tokens;
            }
        }

        (prefill_tps, total_decode_tokens as f64 / slice_elapsed)
    }

    /// Final render - keep last state, don't clear
    pub fn final_render(&mut self) {
        let now = Instant::now();
        // Collect any remaining bucket
        self.collect_bucket(now);
        if !self.silent {
            // Do one last render
            self.render(now);
            println!();
        }
    }

    /// Collect final results from all requests
    pub fn collect_results(&self) -> Vec<LiveTestResult> {
        let mut results = Vec::new();
        for req in &self.requests {
            if req.start_time.is_none() {
                continue;
            }
            let start = req.start_time.unwrap();
            let end = req.end_time.unwrap_or_else(Instant::now);
            let total_dur = end.duration_since(start).as_secs_f64();

            let prefill_dur = req.first_token_time
                .map(|ft| ft.duration_since(start).as_secs_f64())
                .unwrap_or(total_dur);

            let decode_dur = total_dur - prefill_dur;

            results.push(LiveTestResult {
                request_id: req.request_id,
                start,
                prefilled: req.first_token_time.is_some(),
                context_size: req.context_size,
                prompt_tokens: req.prompt_tokens,
                completion_tokens: req.completion_tokens,
                prefill_duration_secs: prefill_dur,
                decode_duration_secs: decode_dur.max(0.0),
                total_duration_secs: total_dur,
                success: req.success,
                error: req.error.clone(),
            });
        }
        results
    }
}
