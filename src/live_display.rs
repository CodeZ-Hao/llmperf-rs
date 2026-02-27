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
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub prefill_duration_secs: f64,
    pub decode_duration_secs: f64,
    pub total_duration_secs: f64,
    pub success: bool,
    pub error: Option<String>,
}

const SPINNER_CHARS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];


impl RequestState {
    pub fn new(request_id: usize) -> Self {
        Self {
            request_id,
            start_time: None,
            first_token_time: None,
            end_time: None,
            prompt_tokens: 0,
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
            TokenEvent::RequestStarted { request_id, start_time, prompt_tokens } => {
                if let Some(req) = self.requests.get_mut(request_id) {
                    req.start_time = Some(start_time);
                    req.prompt_tokens = prompt_tokens;
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

    /// Collect a time-slice bucket from current state
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
            if req.start_time.is_none() {
                continue;
            }
            if req.is_prefill() {
                prefill_active += 1;
                prefill_input_tokens += req.prompt_tokens as f64;
            } else if req.is_decode() || (req.completed && req.slice_tokens > 0) {
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
        let col_status = 12;
        let col_tps = 14;
        let col_tokens = 10;
        let col_time = 10;

        let (lbl_id, lbl_status, lbl_tps, lbl_tokens, lbl_time) =
            if self.lang == "zh" {
                ("#", "状态", "吞吐(t/s)", "输出Token", "耗时(s)")
            } else {
                ("#", "Status", "Tput(t/s)", "Out Toks", "Time(s)")
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
            " {} | {} | {} | {} | {}",
            pad_center(lbl_id, col_id),
            pad_center(lbl_status, col_status),
            pad_center(lbl_tps, col_tps),
            pad_center(lbl_tokens, col_tokens),
            pad_center(lbl_time, col_time),
        );
        lines.push(header.clone());
        lines.push("-".repeat(display_width(&header)));

        // Request rows
        for req in &self.requests {
            let id_str = format!("{}", req.request_id + 1);
            let (status_str, tps_str) = self.format_request_status(req, slice_elapsed, now);
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
                " {} | {} | {} | {} | {}",
                pad_center(&id_str, col_id),
                pad_center(&status_str, col_status),
                pad_left(&tps_str, col_tps),
                pad_left(&tokens_str, col_tokens),
                pad_left(&time_str, col_time),
            );
            lines.push(row);
        }

        // Separator
        lines.push("-".repeat(display_width(&header)));

        // System-wide aggregate for current slice
        let (sys_prefill, sys_decode) = self.calc_system_throughput(slice_elapsed, now);
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

    /// Format a single request's status and throughput for display
    fn format_request_status(&self, req: &RequestState, slice_elapsed: f64, now: Instant) -> (String, String) {
        if req.start_time.is_none() {
            let lbl = if self.lang == "zh" { "等待" } else { "Wait" };
            return (lbl.to_string(), "-".to_string());
        }

        if req.completed {
            if req.success {
                let lbl = if self.lang == "zh" { "完成" } else { "Done" };
                let tps = match req.final_decode_tps {
                    Some(v) => format!("{:.1}", v),
                    None => "-".to_string(),
                };
                return (lbl.to_string(), tps);
            } else {
                let lbl = if self.lang == "zh" { "失败" } else { "Fail" };
                return (lbl.to_string(), "-".to_string());
            }
        }

        if req.is_prefill() {
            let spinner = SPINNER_CHARS[self.spinner_idx];
            let lbl = if self.lang == "zh" {
                format!("{} Prefill", spinner)
            } else {
                format!("{} Prefill", spinner)
            };
            return (lbl, "-".to_string());
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
            return (lbl, tps);
        }

        ("-".to_string(), "-".to_string())
    }

    /// Calculate system-wide throughput for current slice
    /// Prefill: total input tokens / wall-clock prefill time (from completed prefills)
    /// Decode: total output tokens in slice / wall-clock slice time
    fn calc_system_throughput(&self, slice_elapsed: f64, _now: Instant) -> (f64, f64) {
        if slice_elapsed < 0.01 {
            return (0.0, 0.0);
        }

        // Prefill: use completed prefill durations
        let mut total_prefill_tokens: f64 = 0.0;
        let mut max_prefill_time: f64 = 0.0;

        // Decode: sum all tokens received in this slice / wall-clock slice time
        let mut total_decode_tokens: u32 = 0;

        for req in &self.requests {
            if req.start_time.is_none() {
                continue;
            }

            // Prefill: track max prefill duration (concurrent requests overlap)
            if let (Some(start), Some(first_tok)) = (req.start_time, req.first_token_time) {
                let prefill_dur = first_tok.duration_since(start).as_secs_f64();
                total_prefill_tokens += req.prompt_tokens as f64;
                if prefill_dur > max_prefill_time {
                    max_prefill_time = prefill_dur;
                }
            }

            // Decode: sum all tokens from all decoding requests in this slice
            if req.is_decode() || (req.completed && req.slice_tokens > 0) {
                total_decode_tokens += req.slice_tokens;
            }
        }

        let prefill_tps = if max_prefill_time > 0.001 {
            total_prefill_tokens / max_prefill_time
        } else {
            0.0
        };

        // System-wide decode = total tokens / wall-clock time
        let decode_tps = if total_decode_tokens > 0 {
            total_decode_tokens as f64 / slice_elapsed
        } else {
            0.0
        };

        (prefill_tps, decode_tps)
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
