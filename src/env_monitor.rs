use sysinfo::System;
use std::process::Command;

pub struct EnvMonitor;

impl EnvMonitor {
    pub fn collect_with_lang(lang: &str) -> String {
        let mut output = String::new();

        // System info
        let sys = System::new_all();

        // OS
        let os = format!("{} {}", System::name().unwrap_or_default(), System::os_version().unwrap_or_default());
        let (lbl_os, lbl_cpu, lbl_cores, lbl_memory, lbl_gpu) = if lang == "zh" {
            ("操作系统", "CPU", "核心", "内存", "GPU")
        } else {
            ("OS", "CPU", "cores", "Memory", "GPU")
        };

        output.push_str(&format!("{}: {}\n", lbl_os, os));

        // CPU
        let cpu_model = sys.cpus().first().map(|c| c.brand().to_string()).unwrap_or_else(|| "Unknown".to_string());
        let cpu_cores = sys.cpus().len();
        output.push_str(&format!("{}: {} ({} {})\n", lbl_cpu, cpu_model, cpu_cores, lbl_cores));

        // Memory - show detailed config
        let mem_info = Self::format_memory_info();
        output.push_str(&format!("{}: {}\n", lbl_memory, mem_info));

        // GPU info
        #[cfg(target_os = "linux")]
        {
            if let Ok(gpu_info) = Self::get_nvidia_info() {
                for (key, value) in gpu_info {
                    output.push_str(&format!("{} {}: {}\n", lbl_gpu, key, value));
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(gpu_info) = Self::get_gpu_info_windows() {
                for (key, value) in gpu_info {
                    output.push_str(&format!("{} {}: {}\n", lbl_gpu, key, value));
                }
            }
        }

        output
    }

    fn format_memory_info() -> String {
        #[cfg(target_os = "linux")]
        {
            // Use dmidecode to get exact memory DIMM configuration on Linux
            if let Ok(output) = Command::new("dmidecode")
                .arg("-t")
                .arg("memory")
                .output()
            {
                if output.status.success() {
                    let output_str = String::from_utf8_lossy(&output.stdout);
                    let mut dimm_sizes: Vec<u32> = Vec::new();

                    for line in output_str.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("Size:") && !trimmed.contains("No Module") {
                            if let Some(size_str) = trimmed.split_whitespace().nth(1) {
                                if let Ok(size) = size_str.parse::<u32>() {
                                    if size > 0 {
                                        dimm_sizes.push(size);
                                    }
                                }
                            }
                        }
                    }

                    if !dimm_sizes.is_empty() {
                        return Self::format_dimm_config(&dimm_sizes);
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Use wmic on Windows to get memory configuration
            if let Ok(output) = Command::new("wmic")
                .args(["memorychip", "get", "Capacity"])
                .output()
            {
                if output.status.success() {
                    let output_str = String::from_utf8_lossy(&output.stdout);
                    let mut dimm_sizes: Vec<u32> = Vec::new();

                    for line in output_str.lines().skip(1) {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            if let Ok(capacity) = trimmed.parse::<u64>() {
                                let size_gb = capacity / (1024 * 1024 * 1024) as u64;
                                if size_gb > 0 {
                                    dimm_sizes.push(size_gb as u32);
                                }
                            }
                        }
                    }

                    if !dimm_sizes.is_empty() {
                        return Self::format_dimm_config(&dimm_sizes);
                    }
                }
            }
        }

        // Fallback to simple display using sysinfo
        let sys = System::new_all();
        let total_mem = sys.total_memory() / (1024 * 1024 * 1024);
        let available_mem = sys.available_memory() / (1024 * 1024 * 1024);
        format!("{} GB total, {} GB available", total_mem, available_mem)
    }

    fn format_dimm_config(dimm_sizes: &[u32]) -> String {
        if dimm_sizes.is_empty() {
            return String::new();
        }

        // Group by size and count
        let mut size_counts: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for size in dimm_sizes {
            *size_counts.entry(*size).or_insert(0) += 1;
        }

        let mut config = String::new();
        let mut sizes: Vec<u32> = size_counts.keys().cloned().collect();
        sizes.sort_by(|a, b| b.cmp(a)); // Sort descending

        for size in sizes {
            if let Some(&count) = size_counts.get(&size) {
                if !config.is_empty() {
                    config.push_str(" ");
                }
                config.push_str(&format!("{}G*{}", size, count));
            }
        }

        config
    }

    #[cfg(target_os = "linux")]
    fn get_nvidia_info() -> Result<Vec<(String, String)>, String> {
        let mut info = Vec::new();

        // Try nvidia-smi first - 只获取内存信息，不获取利用率
        if let Ok(output) = Command::new("nvidia-smi")
            .arg("--query-gpu=index,name,memory.total")
            .arg("--format=csv,noheader,nounits")
            .output()
        {
            if output.status.success() {
                let output_str = String::from_utf8_lossy(&output.stdout);
                let lines: Vec<&str> = output_str.trim().lines().collect();

                for line in lines {
                    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                    if parts.len() >= 3 {
                        let index = parts[0];
                        let name = parts[1];
                        let mem_total: u64 = parts[2].parse().unwrap_or(0);
                        // 直接显示MB值，不做计算
                        let mem_display = format!("{}MB", mem_total);

                        info.push((
                            format!("{}", index),
                            format!("{} {}", name, mem_display),
                        ));
                    }
                }
                return Ok(info);
            }
        }

        Err("No GPU info available".to_string())
    }

    #[cfg(target_os = "windows")]
    fn get_gpu_info_windows() -> Result<Vec<(String, String)>, String> {
        let mut info = Vec::new();

        // Try wmic to get GPU info on Windows
        if let Ok(output) = Command::new("wmic")
            .args(["path", "win32_VideoController", "get", "name,adapterram"])
            .output()
        {
            if output.status.success() {
                let output_str = String::from_utf8_lossy(&output.stdout);
                for line in output_str.lines().skip(1) {
                    let parts: Vec<&str> = line.trim().split_whitespace().collect();
                    if parts.len() >= 2 {
                        let name = parts[0..parts.len()-1].join(" ");
                        let ram = parts.last().unwrap_or(&"0");
                        if let Ok(ram_bytes) = ram.parse::<u64>() {
                            let mem_gb = ram_bytes / (1024 * 1024 * 1024);
                            if !name.is_empty() && mem_gb > 0 {
                                info.push((
                                    format!("{}", info.len() + 1),
                                    format!("{} {}G", name, mem_gb),
                                ));
                            }
                        }
                    }
                }
            }
        }

        if info.is_empty() {
            return Err("No GPU info available".to_string());
        }

        Ok(info)
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    fn get_nvidia_info() -> Result<Vec<(String, String)>, String> {
        Err("GPU info not available on this platform".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_dimm_config_uniform() {
        assert_eq!(EnvMonitor::format_dimm_config(&[64, 64, 64, 64]), "64G*4");
    }

    #[test]
    fn test_format_dimm_config_mixed() {
        let result = EnvMonitor::format_dimm_config(&[64, 64, 32]);
        assert!(result.contains("64G*2"));
        assert!(result.contains("32G*1"));
    }

    #[test]
    fn test_format_dimm_config_empty() {
        assert_eq!(EnvMonitor::format_dimm_config(&[]), "");
    }
}
