use crate::live_display::LiveTestResult;
use crate::utils::pad_center;
use serde_json::json;
use std::time::{Duration, Instant};


/// Print final aggregate results showing system-wide throughput
pub fn print_final_results(results: &[LiveTestResult], lang: &str) {
    let success: Vec<&LiveTestResult> = results.iter().filter(|r| r.success).collect();
    let failed: Vec<&LiveTestResult> = results.iter().filter(|r| !r.success).collect();

    let (lbl_results, lbl_total, lbl_success, lbl_failed) = if lang == "zh" {
        ("测试结果", "总请求", "成功", "失败")
    } else {
        ("Test Results", "Total", "Success", "Failed")
    };

    println!("=== {} ===", lbl_results);
    println!("{}: {}", lbl_total, results.len());
    println!("{}: {}", lbl_success, success.len());
    println!("{}: {}", lbl_failed, failed.len());

    if !success.is_empty() {
        print_system_throughput(&success, lang);
    }

    if !failed.is_empty() {
        print_failed_requests(&failed, lang);
    }
}

/// Print system-wide aggregate throughput
fn print_system_throughput(results: &[&LiveTestResult], lang: &str) {
    let (total_prompt_tokens, total_completion_tokens, total_time,
         sys_prefill_tps, sys_decode_tps) = calc_throughput_stats(results);

    let (lbl_sys, lbl_prefill, lbl_decode, lbl_total_time,
         lbl_prompt_tok, lbl_compl_tok) = if lang == "zh" {
        ("系统吞吐", "Prefill", "Decode", "总耗时",
         "输入Token总计", "输出Token总计")
    } else {
        ("System Throughput", "Prefill", "Decode", "Total time",
         "Total input tokens", "Total output tokens")
    };

    println!("\n--- {} ---", lbl_sys);
    println!("{}: {:.0}", lbl_prompt_tok, total_prompt_tokens);
    println!("{}: {:.0}", lbl_compl_tok, total_completion_tokens);
    println!("{}: {:.2} input tok/s", lbl_prefill, sys_prefill_tps);
    println!("{}: {:.2} output tok/s", lbl_decode, sys_decode_tps);
    println!("{}: {:.2}s", lbl_total_time, total_time);
    println!();
}

/// Print failed request details
fn print_failed_requests(results: &[&LiveTestResult], lang: &str) {
    let (lbl_failed_req, lbl_id, lbl_error) = if lang == "zh" {
        ("失败请求", "#", "错误")
    } else {
        ("Failed Requests", "#", "Error")
    };

    println!("\n=== {} ===", lbl_failed_req);
    let col_id = 6;
    let header = format!(
        "{} | {}",
        pad_center(lbl_id, col_id),
        lbl_error
    );
    println!("{}", header);
    println!("{}", "-".repeat(60));

    for r in results {
        let error = r.error.as_deref().unwrap_or("Unknown error");
        let error = if error.len() > 50 { &error[..50] } else { error };
        println!(
            "{} | {}",
            pad_center(&format!("{}", r.request_id + 1), col_id),
            error
        );
    }
    println!();
}

/// Build JSON output for test results
pub fn build_json_results(
    results: &[LiveTestResult],
    model: &str,
    concurrent: usize,
    max_tokens: u32,
    env_monitor: bool,
    lang: &str,
) -> String {
    let success: Vec<&LiveTestResult> = results.iter().filter(|r| r.success).collect();
    let failed: Vec<&LiveTestResult> = results.iter().filter(|r| !r.success).collect();

    let (total_prompt_tokens, total_completion_tokens, total_time,
         sys_prefill_tps, sys_decode_tps) = if !success.is_empty() {
        calc_throughput_stats(&success)
    } else {
        (0.0, 0.0, 0.0, 0.0, 0.0)
    };

    let requests_json: Vec<serde_json::Value> = results.iter().map(|r| {
        let prefill_tps = if r.prefilled && r.prefill_duration_secs > 0.001 {
            r.prompt_tokens as f64 / r.prefill_duration_secs
        } else {
            0.0
        };
        json!({
            "request_id": r.request_id + 1,
            "success": r.success,
            "context_size": r.context_size,
            "prompt_tokens": r.prompt_tokens,
            "completion_tokens": r.completion_tokens,
            "prefill_tok_per_sec": round2(prefill_tps),
            "prefill_duration_secs": round2(r.prefill_duration_secs),
            "decode_duration_secs": round2(r.decode_duration_secs),
            "total_duration_secs": round2(r.total_duration_secs),
            "error": r.error,
        })
    }).collect();

    let mut output = json!({
        "model": model,
        "concurrent": concurrent,
        "max_tokens": max_tokens,
        "total": results.len(),
        "success": success.len(),
        "failed": failed.len(),
        "system_throughput": {
            "total_input_tokens": total_prompt_tokens as u64,
            "total_output_tokens": total_completion_tokens as u64,
            "prefill_tok_per_sec": round2(sys_prefill_tps),
            "decode_tok_per_sec": round2(sys_decode_tps),
            "total_time_secs": round2(total_time),
        },
        "requests": requests_json,
    });

    if env_monitor {
        let env_info = crate::env_monitor::EnvMonitor::collect_with_lang(lang);
        output["environment"] = json!(env_info);
    }

    serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
}

/// System throughput = total tokens / the union of the busy intervals.
/// This accumulates tokens over the actual busy timeline: for sequential
/// (single-concurrency) runs the union is the sum of per-request durations,
/// so throughput is the time-averaged rate — never the sum of per-request
/// rates. For parallel runs the union is the wall-clock span, which yields
/// the true system-level rate.
fn calc_throughput_stats(results: &[&LiveTestResult]) -> (f64, f64, f64, f64, f64) {
    let total_prompt_tokens: f64 = results.iter().map(|r| r.prompt_tokens as f64).sum();
    let total_completion_tokens: f64 = results.iter().map(|r| r.completion_tokens as f64).sum();

    let prefill_intervals: Vec<(Instant, Instant)> = results.iter()
        .filter(|r| r.prefilled && r.prefill_duration_secs > 0.001)
        .map(|r| (
            r.start,
            r.start + Duration::from_secs_f64(r.prefill_duration_secs),
        ))
        .collect();

    let decode_intervals: Vec<(Instant, Instant)> = results.iter()
        .filter(|r| r.prefilled && r.decode_duration_secs > 0.001)
        .map(|r| (
            r.start + Duration::from_secs_f64(r.prefill_duration_secs),
            r.start + Duration::from_secs_f64(r.total_duration_secs),
        ))
        .collect();

    let full_intervals: Vec<(Instant, Instant)> = results.iter()
        .map(|r| (
            r.start,
            r.start + Duration::from_secs_f64(r.total_duration_secs),
        ))
        .collect();

    let prefill_span = union_duration(prefill_intervals);
    let decode_span = union_duration(decode_intervals);
    let total_time = union_duration(full_intervals);

    let sys_prefill_tps = if prefill_span > 0.001 {
        total_prompt_tokens / prefill_span
    } else {
        0.0
    };

    let sys_decode_tps = if decode_span > 0.001 {
        total_completion_tokens / decode_span
    } else {
        0.0
    };

    (total_prompt_tokens, total_completion_tokens, total_time, sys_prefill_tps, sys_decode_tps)
}

/// Length of the union of intervals (wall-clock time during which at least one
/// interval was active). Sequential intervals add up; overlapping ones merge.
fn union_duration(mut intervals: Vec<(Instant, Instant)>) -> f64 {
    if intervals.is_empty() {
        return 0.0;
    }
    intervals.sort_unstable_by_key(|(s, _)| *s);
    let mut total = Duration::ZERO;
    let mut cur_start = intervals[0].0;
    let mut cur_end = intervals[0].1;
    for &(s, e) in intervals.iter().skip(1) {
        if s > cur_end {
            total += cur_end - cur_start;
            cur_start = s;
            cur_end = e;
        } else if e > cur_end {
            cur_end = e;
        }
    }
    total += cur_end - cur_start;
    total.as_secs_f64()
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
