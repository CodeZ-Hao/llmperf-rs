use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// System-wide default config path on Linux
const SYSTEM_CONFIG_PATH: &str = "/etc/llmperf-rs/llmperf-rs.yaml";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    #[serde(default = "default_model", alias = "default_model")]
    pub model: String,
    #[serde(skip)]
    pub lang: String,
    #[serde(default = "default_time_slice")]
    pub time_slice_interval: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<TestConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct TestConfig {
    pub concurrent: Option<usize>,
    pub context: Option<String>,
    pub max_tokens: Option<u32>,
    pub env_monitor: Option<bool>,
    pub time_slice: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct ChatConfig {
    pub max_tokens: Option<u32>,
    pub prompt: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: None,
            api_key: None,
            model: default_model(),
            lang: "en".to_string(),
            time_slice_interval: default_time_slice(),
            test: None,
            chat: None,
        }
    }
}

fn default_time_slice() -> f64 {
    3.0
}

fn default_model() -> String {
    "gpt-4".to_string()
}

/// CLI overrides for config values
pub struct CliOverrides {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file: {}", e))?;
        let mut config: Config = serde_yaml::from_str(&content)
            .map_err(|e| format!("Failed to parse config file: {}", e))?;
        config.lang = detect_system_lang();
        Ok(config)
    }

    /// Resolve config: CLI/env (already merged by clap) > config file > interactive prompt.
    pub fn resolve(path: &Path, overrides: &CliOverrides) -> Result<Self, String> {
        let resolved_path = if path.exists() {
            Some(path.to_path_buf())
        } else {
            let system_path = Path::new(SYSTEM_CONFIG_PATH);
            if system_path.exists() { Some(system_path.to_path_buf()) } else { None }
        };

        let mut config = if let Some(p) = &resolved_path {
            Self::load(p)?
        } else {
            let mut c = Config::default();
            c.lang = detect_system_lang();
            c
        };

        // CLI/env overrides take priority over config file
        if let Some(url) = &overrides.base_url {
            config.base_url = Some(url.clone());
        }
        if let Some(key) = &overrides.api_key {
            config.api_key = Some(key.clone());
        }

        // If still missing credentials, prompt interactively
        if config.base_url.is_none() || config.api_key.is_none() {
            Self::prompt_credentials(&mut config)?;
            Self::ask_save_config(&config)?;
        }

        Ok(config)
    }

    fn prompt_credentials(config: &mut Config) -> Result<(), String> {
        let lang = &config.lang;
        if config.base_url.is_none() {
            let default_url = "https://api.openai.com/v1";
            print!("{} [default: {}]: ",
                if lang == "zh" { "API 地址" } else { "Base URL" }, default_url);
            io::stdout().flush().unwrap();
            let mut input = String::new();
            io::stdin().read_line(&mut input).unwrap();
            let input = input.trim();
            config.base_url = Some(if input.is_empty() { default_url.to_string() } else { input.to_string() });
        }
        if config.api_key.is_none() {
            print!("API Key: ");
            io::stdout().flush().unwrap();
            let mut input = String::new();
            io::stdin().read_line(&mut input).unwrap();
            let input = input.trim().to_string();
            if input.is_empty() {
                let msg = if lang == "zh" { "API Key 不能为空" } else { "API Key is required" };
                return Err(msg.to_string());
            }
            config.api_key = Some(input);
        }
        Ok(())
    }

    fn ask_save_config(config: &Config) -> Result<(), String> {
        let prompt = if config.lang == "zh" { "是否保存配置文件? (y/N): " } else { "Save config file? (y/N): " };
        print!("{}", prompt);
        io::stdout().flush().unwrap();
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        if input.trim().eq_ignore_ascii_case("y") {
            let default_path = PathBuf::from("llmperf-rs.yaml");
            print!("{} [default: {}]: ",
                if config.lang == "zh" { "保存路径" } else { "Save path" },
                default_path.display());
            io::stdout().flush().unwrap();
            let mut path_input = String::new();
            io::stdin().read_line(&mut path_input).unwrap();
            let path_input = path_input.trim();
            let save_path = if path_input.is_empty() { default_path } else { PathBuf::from(path_input) };
            Self::write_config(&save_path, config)?;
            println!("{}: {}",
                if config.lang == "zh" { "配置文件已保存" } else { "Config saved" },
                save_path.display());
        }
        Ok(())
    }

    pub fn write_config(path: &Path, config: &Config) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
            }
        }
        let content = serde_yaml::to_string(config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        fs::write(path, content)
            .map_err(|e| format!("Failed to write config file {}: {}", path.display(), e))
    }
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
    fn test_config_serialization() {
        let config = Config {
            base_url: Some("https://api.example.com/v1".to_string()),
            api_key: Some("sk-test123".to_string()),
            model: "gpt-4".to_string(),
            lang: "en".to_string(),
            time_slice_interval: 3.0,
            test: Some(TestConfig {
                concurrent: Some(4),
                context: Some("4096".to_string()),
                max_tokens: Some(128),
                env_monitor: Some(false),
                time_slice: None,
            }),
            chat: Some(ChatConfig {
                max_tokens: Some(1024),
                prompt: Some("hello".to_string()),
            }),
        };

        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(yaml.contains("base_url: https://api.example.com/v1"));
        assert!(yaml.contains("concurrent: 4"));
        assert!(yaml.contains("prompt: hello"));
    }

    #[test]
    fn test_config_deserialization() {
        let yaml = r#"
base_url: "https://api.example.com/v1"
api_key: "sk-test123"
model: "gpt-4"
time_slice_interval: 3.0

test:
  concurrent: 4
  context: "4096"
  max_tokens: 128

chat:
  max_tokens: 1024
  prompt: "hello"
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(config.base_url, Some("https://api.example.com/v1".to_string()));
        assert_eq!(config.api_key, Some("sk-test123".to_string()));
        assert_eq!(config.model, "gpt-4");

        let test_config = config.test.unwrap();
        assert_eq!(test_config.concurrent, Some(4));
        assert_eq!(test_config.context, Some("4096".to_string()));
        assert_eq!(test_config.max_tokens, Some(128));

        let chat_config = config.chat.unwrap();
        assert_eq!(chat_config.max_tokens, Some(1024));
        assert_eq!(chat_config.prompt, Some("hello".to_string()));
    }

    #[test]
    fn test_config_partial_fields() {
        let yaml = r#"
base_url: "https://api.example.com/v1"
api_key: "sk-test123"
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(config.base_url, Some("https://api.example.com/v1".to_string()));
        assert_eq!(config.api_key, Some("sk-test123".to_string()));
        assert_eq!(config.model, "gpt-4"); // default
        assert_eq!(config.test, None);
        assert_eq!(config.chat, None);
    }

    #[test]
    fn test_chat_config_only() {
        let yaml = r#"
base_url: "https://api.example.com/v1"
api_key: "sk-test123"

chat:
  prompt: "test prompt"
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();

        assert!(config.test.is_none());
        assert!(config.chat.is_some());
        assert_eq!(config.chat.as_ref().unwrap().prompt, Some("test prompt".to_string()));
    }
}

