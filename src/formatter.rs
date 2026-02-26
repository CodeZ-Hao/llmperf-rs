use crate::live_display::LiveTestResult;
use crate::utils::pad_center;


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
    let total_prompt_tokens: f64 = results.iter().map(|r| r.prompt_tokens as f64).sum();
    let total_completion_tokens: f64 = results.iter().map(|r| r.completion_tokens as f64).sum();

    // Wall-clock time = max of all request durations (they run concurrently)
    let total_time: f64 = results.iter()
        .map(|r| r.total_duration_secs)
        .fold(0.0_f64, |a, b| a.max(b));

    // System-wide prefill: total input tokens / max prefill duration (concurrent overlap)
    let max_prefill_time: f64 = results.iter()
        .map(|r| r.prefill_duration_secs)
        .fold(0.0_f64, |a, b| a.max(b));

    let sys_prefill_tps = if max_prefill_time > 0.001 {
        total_prompt_tokens / max_prefill_time
    } else {
        0.0
    };

    // System-wide decode: total output tokens / max decode duration (concurrent overlap)
    // Use per-request decode durations directly instead of approximating from total - prefill
    let max_decode_time: f64 = results.iter()
        .map(|r| r.decode_duration_secs)
        .fold(0.0_f64, |a, b| a.max(b));

    let sys_decode_tps = if max_decode_time > 0.001 {
        total_completion_tokens / max_decode_time
    } else {
        0.0
    };

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
