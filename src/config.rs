use serde::Deserialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// System-wide default config path on Linux
const SYSTEM_CONFIG_PATH: &str = "/etc/llmperf/config.yaml";

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub base_url: String,
    pub api_key: String,
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default = "default_time_slice")]
    pub time_slice_interval: f64,
}

fn default_time_slice() -> f64 {
    3.0
}

fn default_model() -> String {
    "gpt-4".to_string()
}

fn default_lang() -> String {
    "en".to_string()
}

/// CLI overrides for config values
pub struct CliOverrides {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub config_explicit: bool,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file: {}", e))?;
        let config: Config = serde_yaml::from_str(&content)
            .map_err(|e| format!("Failed to parse config file: {}", e))?;
        Ok(config)
    }

    /// Resolve config with priority: CLI args > ./config.yaml > /etc/llmperf/config.yaml > env vars.
    /// When no config file exists:
    ///   - If CLI args provided, write them to config.yaml
    ///   - If env vars exist, prompt user with env var values as defaults
    ///   - Otherwise, prompt user interactively
    pub fn resolve(path: &Path, overrides: &CliOverrides) -> Result<Self, String> {
        // Try explicit path first, then fallback to /etc/llmperf/config.yaml
        let resolved_path = if path.exists() {
            path.to_path_buf()
        } else {
            let system_path = Path::new("/etc/llmperf/config.yaml");
            if system_path.exists() {
                system_path.to_path_buf()
            } else {
                path.to_path_buf() // will fall through to interactive setup
            }
        };

        if resolved_path.exists() {
            let mut config = Self::load(&resolved_path)?;
            // Apply CLI overrides on top of config file
            if let Some(url) = &overrides.base_url {
                config.base_url = url.clone();
            }
            if let Some(key) = &overrides.api_key {
                config.api_key = key.clone();
            }
            return Ok(config);
        }

        // No config file found — determine save path
        // If user explicitly specified --config, respect that; otherwise default to /etc/llmperf/
        let save_path = if overrides.config_explicit {
            path.to_path_buf()
        } else {
            PathBuf::from(SYSTEM_CONFIG_PATH)
        };

        let env_base_url = std::env::var("OPENAI_BASE_URL").ok();
        let env_api_key = std::env::var("OPENAI_API_KEY").ok();

        let has_cli_args = overrides.base_url.is_some() && overrides.api_key.is_some();
        let has_env = env_base_url.is_some() && env_api_key.is_some();

        if has_cli_args {
            // CLI args provided — still prompt for remaining config fields
            let detected_lang = detect_system_lang();
            let base_url = overrides.base_url.clone().unwrap();
            let api_key = overrides.api_key.clone().unwrap();

            println!("\nConfiguration file not found");
            println!("Will save to: {}", save_path.display());
            println!("\n=== Create New Configuration / 创建新配置文件 ===\n");

            print!("{} [default: gpt-4]: ",
                if detected_lang == "zh" { "默认模型" } else { "Default Model" }
            );
            io::stdout().flush().unwrap();
            let mut model = String::new();
            io::stdin().read_line(&mut model).unwrap();
            model = model.trim().to_string();
            if model.is_empty() {
                model = default_model();
            }

            print!("{} (zh/en) [default: {}]: ",
                if detected_lang == "zh" { "语言" } else { "Language" },
                detected_lang
            );
            io::stdout().flush().unwrap();
            let mut chosen_lang = String::new();
            io::stdin().read_line(&mut chosen_lang).unwrap();
            chosen_lang = chosen_lang.trim().to_string();
            if chosen_lang.is_empty() {
                chosen_lang = detected_lang;
            }
            if chosen_lang != "zh" && chosen_lang != "en" {
                chosen_lang = "en".to_string();
            }

            let config = Config {
                base_url,
                api_key,
                default_model: model,
                lang: chosen_lang,
                time_slice_interval: default_time_slice(),
            };
            Self::write_config(&save_path, &config)?;
            return Ok(config);
        }

        // Interactive setup — use env vars as defaults if available
        let detected_lang = detect_system_lang();
        println!("\nConfiguration file not found");
        println!("Will save to: {}", save_path.display());
        if has_env {
            if detected_lang == "zh" {
                println!("检测到环境变量 OPENAI_BASE_URL / OPENAI_API_KEY，将作为默认值");
            } else {
                println!("Detected OPENAI_BASE_URL / OPENAI_API_KEY env vars, using as defaults");
            }
        }
        println!("\n=== Create New Configuration / 创建新配置文件 ===\n");

        let config = Self::interactive_create(&detected_lang, &env_base_url, &env_api_key)?;
        Self::write_config(&save_path, &config)?;

        if detected_lang == "zh" {
            println!("\n配置文件创建成功: {}", save_path.display());
        } else {
            println!("\nConfiguration file created: {}", save_path.display());
        }
        Ok(config)
    }

    fn interactive_create(
        lang: &str,
        env_base_url: &Option<String>,
        env_api_key: &Option<String>,
    ) -> Result<Self, String> {
        let default_url = env_base_url
            .as_deref()
            .unwrap_or("https://api.openai.com/v1");

        print!("{} [default: {}]: ",
            if lang == "zh" { "API 地址" } else { "Base URL" },
            default_url
        );
        io::stdout().flush().unwrap();
        let mut base_url = String::new();
        io::stdin().read_line(&mut base_url).unwrap();
        base_url = base_url.trim().to_string();
        if base_url.is_empty() {
            base_url = default_url.to_string();
        }

        // API key — use env var as default if available
        if let Some(env_key) = env_api_key {
            let masked = mask_key(env_key);
            print!("API Key [default: {}]: ", masked);
        } else {
            print!("API Key: ");
        }
        io::stdout().flush().unwrap();
        let mut api_key = String::new();
        io::stdin().read_line(&mut api_key).unwrap();
        api_key = api_key.trim().to_string();
        if api_key.is_empty() {
            if let Some(env_key) = env_api_key {
                api_key = env_key.clone();
            } else {
                let msg = if lang == "zh" { "API Key 不能为空" } else { "API Key is required" };
                return Err(msg.to_string());
            }
        }

        print!("{} [default: gpt-4]: ",
            if lang == "zh" { "默认模型" } else { "Default Model" }
        );
        io::stdout().flush().unwrap();
        let mut model = String::new();
        io::stdin().read_line(&mut model).unwrap();
        model = model.trim().to_string();
        if model.is_empty() {
            model = default_model();
        }

        print!("{} (zh/en) [default: {}]: ",
            if lang == "zh" { "语言" } else { "Language" },
            lang
        );
        io::stdout().flush().unwrap();
        let mut chosen_lang = String::new();
        io::stdin().read_line(&mut chosen_lang).unwrap();
        chosen_lang = chosen_lang.trim().to_string();
        if chosen_lang.is_empty() {
            chosen_lang = lang.to_string();
        }
        if chosen_lang != "zh" && chosen_lang != "en" {
            chosen_lang = "en".to_string();
        }

        Ok(Config {
            base_url,
            api_key,
            default_model: model,
            lang: chosen_lang,
            time_slice_interval: default_time_slice(),
        })
    }

    fn write_config(path: &Path, config: &Config) -> Result<(), String> {
        // Auto-create parent directories if needed (e.g. /etc/llmperf/)
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
            }
        }
        let content = format!(
            "base_url: \"{}\"\napi_key: \"{}\"\ndefault_model: \"{}\"\nlang: \"{}\"\ntime_slice_interval: {}\n",
            config.base_url, config.api_key, config.default_model, config.lang, config.time_slice_interval
        );
        fs::write(path, content)
            .map_err(|e| format!("Failed to write config file {}: {}", path.display(), e))
    }
}

/// Mask an API key for display: show first 6 and last 4 chars
fn mask_key(key: &str) -> String {
    if key.len() <= 10 {
        return "****".to_string();
    }
    format!("{}...{}", &key[..6], &key[key.len()-4..])
}

/// Detect system default language
pub fn detect_system_lang() -> String {
    #[cfg(target_os = "linux")]
    {
        for var in &["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(val) = std::env::var(var) {
                if val.starts_with("zh") {
                    return "zh".to_string();
                }
            }
        }
        if let Ok(output) = Command::new("locale").output() {
            let output = String::from_utf8_lossy(&output.stdout);
            if output.contains("zh") {
                return "zh".to_string();
            }
        }
        if let Ok(output) = fs::read_to_string("/etc/locale.conf") {
            if output.contains("zh_CN") || output.contains("zh_TW") {
                return "zh".to_string();
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
        {
            let output = String::from_utf8_lossy(&output.stdout);
            if output.contains("zh") {
                return "zh".to_string();
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = Command::new("powershell")
            .args(["-Command", "[System.Globalization.CultureInfo]::CurrentCulture.Name"])
            .output()
        {
            let output = String::from_utf8_lossy(&output.stdout);
            if output.starts_with("zh") {
                return "zh".to_string();
            }
        }
    }

    "en".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_key_long() {
        assert_eq!(mask_key("sk-abcdef1234567890"), "sk-abc...7890");
    }

    #[test]
    fn test_mask_key_short() {
        assert_eq!(mask_key("short"), "****");
        assert_eq!(mask_key("1234567890"), "****");
    }

    #[test]
    fn test_mask_key_empty() {
        assert_eq!(mask_key(""), "****");
    }
}
