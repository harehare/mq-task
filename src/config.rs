//! Configuration for mq_task task runner

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::path::Path;

use crate::error::{Error, Result};

/// Execution mode for a runtime
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    /// Pass code via stdin
    #[default]
    Stdin,
    /// Write code to a temporary file and pass it as argument
    File,
    /// Pass code as a command argument
    Arg,
}

impl TryFrom<&str> for ExecutionMode {
    type Error = Error;

    fn try_from(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "stdin" => Ok(ExecutionMode::Stdin),
            "file" => Ok(ExecutionMode::File),
            "arg" => Ok(ExecutionMode::Arg),
            _ => Err(Error::Config(format!(
                "Invalid execution mode: '{}'. Valid options: stdin, file, arg",
                s
            ))),
        }
    }
}

/// Runtime configuration that can be either a simple string or a detailed config
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RuntimeConfig {
    /// Simple command string (execution_mode defaults to stdin)
    Simple(String),
    /// Detailed configuration with command and execution mode
    Detailed {
        command: String,
        #[serde(default)]
        execution_mode: ExecutionMode,
    },
}

impl RuntimeConfig {
    /// Get the command string from the runtime config
    pub fn command(&self) -> &str {
        match self {
            RuntimeConfig::Simple(cmd) => cmd,
            RuntimeConfig::Detailed { command, .. } => command,
        }
    }

    /// Get the execution mode from the runtime config
    pub fn execution_mode(&self) -> ExecutionMode {
        match self {
            RuntimeConfig::Simple(_) => ExecutionMode::default(),
            RuntimeConfig::Detailed { execution_mode, .. } => execution_mode.clone(),
        }
    }
}

/// Configuration for mq_task task runner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Runtime mappings: language -> command or detailed config
    #[serde(default = "default_runtimes")]
    pub runtimes: HashMap<String, RuntimeConfig>,
    /// Task run when mq-task is invoked with no task name and no subcommand
    #[serde(default)]
    pub default_task: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runtimes: default_runtimes(),
            default_task: None,
        }
    }
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Get runtime command for a language
    pub fn get_runtime(&self, lang: &str) -> Option<&str> {
        self.runtimes.get(lang).map(|config| config.command())
    }

    /// Get execution mode for a language
    pub fn get_execution_mode(&self, lang: &str) -> ExecutionMode {
        self.runtimes
            .get(lang)
            .map(|config| config.execution_mode())
            .unwrap_or_default()
    }

    /// Check if runtime exists for a language
    pub fn has_runtime(&self, lang: &str) -> bool {
        self.runtimes.contains_key(lang)
    }

    /// Apply runtime overrides from CLI arguments
    /// Format: ["lang:command", "lang2:command2"]
    /// execution_mode: optional execution mode to apply to all overrides
    pub fn apply_runtime_overrides(
        &mut self,
        overrides: &[String],
        execution_mode: Option<ExecutionMode>,
    ) -> Result<()> {
        for override_str in overrides {
            let parts: Vec<&str> = override_str.splitn(2, ':').collect();
            if parts.len() != 2 {
                return Err(Error::Config(format!(
                    "Invalid runtime override format: '{}'. Expected format: 'lang:command'",
                    override_str
                )));
            }

            let lang = parts[0].to_string();
            let command = parts[1].to_string();

            let runtime_config = if let Some(ref mode) = execution_mode {
                RuntimeConfig::Detailed {
                    command,
                    execution_mode: mode.clone(),
                }
            } else {
                RuntimeConfig::Simple(command)
            };

            self.runtimes.insert(lang, runtime_config);
        }

        Ok(())
    }

    /// Validate that all configured runtimes are available in PATH
    pub fn validate_runtimes(&self) -> Result<()> {
        for (lang, config) in &self.runtimes {
            let cmd = config.command();
            let binary = cmd.split_whitespace().next().unwrap_or(cmd);
            if which::which(binary).is_err() {
                return Err(Error::Config(format!(
                    "Runtime '{}' for language '{}' not found in PATH",
                    binary, lang
                )));
            }
        }
        Ok(())
    }
}

/// Default runtime mappings
fn default_runtimes() -> HashMap<String, RuntimeConfig> {
    let mut runtimes = HashMap::new();

    // File mode (default) keeps the child's real stdin free for interactive
    // prompts like `read`; Stdin mode consumes it to deliver the script.
    for (lang, command) in [
        ("bash", "bash"),
        ("sh", "sh"),
        ("python", "python3"),
        ("ruby", "ruby"),
        ("node", "node"),
        ("javascript", "node"),
        ("js", "node"),
        ("php", "php"),
        ("perl", "perl"),
    ] {
        runtimes.insert(
            lang.to_string(),
            RuntimeConfig::Detailed {
                command: command.to_string(),
                execution_mode: ExecutionMode::File,
            },
        );
    }

    // jq's filter is passed as a command-line argument, not read from stdin
    // (stdin is reserved for the JSON data), so it uses arg mode.
    runtimes.insert(
        "jq".to_string(),
        RuntimeConfig::Detailed {
            command: "jq".to_string(),
            execution_mode: ExecutionMode::Arg,
        },
    );

    // Go requires file-based execution
    runtimes.insert(
        "go".to_string(),
        RuntimeConfig::Detailed {
            command: "go run".to_string(),
            execution_mode: ExecutionMode::File,
        },
    );
    runtimes.insert(
        "golang".to_string(),
        RuntimeConfig::Detailed {
            command: "go run".to_string(),
            execution_mode: ExecutionMode::File,
        },
    );

    // mq requires argument-based execution
    runtimes.insert(
        "mq".to_string(),
        RuntimeConfig::Detailed {
            command: "mq".to_string(),
            execution_mode: ExecutionMode::Arg,
        },
    );

    runtimes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_runtime() {
        let config = Config::default();
        assert_eq!(config.get_runtime("bash"), Some("bash"));
        assert_eq!(config.get_runtime("python"), Some("python3"));
        assert_eq!(config.get_runtime("unknown"), None);
    }

    #[test]
    fn test_execution_modes() {
        let config = Config::default();
        // Interpreted languages default to file-based execution so task
        // scripts keep interactive access to the real stdin.
        assert_eq!(config.get_execution_mode("bash"), ExecutionMode::File);
        assert_eq!(config.get_execution_mode("python"), ExecutionMode::File);

        // Test file-based execution mode
        assert_eq!(config.get_execution_mode("go"), ExecutionMode::File);
        assert_eq!(config.get_execution_mode("golang"), ExecutionMode::File);

        // Test arg-based execution mode
        assert_eq!(config.get_execution_mode("mq"), ExecutionMode::Arg);
        assert_eq!(config.get_execution_mode("jq"), ExecutionMode::Arg);
    }

    #[test]
    fn test_runtime_config_simple() {
        let config = RuntimeConfig::Simple("python3".to_string());
        assert_eq!(config.command(), "python3");
        assert_eq!(config.execution_mode(), ExecutionMode::Stdin);
    }

    #[test]
    fn test_runtime_config_detailed() {
        let config = RuntimeConfig::Detailed {
            command: "go run".to_string(),
            execution_mode: ExecutionMode::File,
        };
        assert_eq!(config.command(), "go run");
        assert_eq!(config.execution_mode(), ExecutionMode::File);
    }

    #[test]
    fn test_toml_deserialization_simple() {
        let toml = r#"
[runtimes]
python = "python3"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.get_runtime("python"), Some("python3"));
        assert_eq!(config.get_execution_mode("python"), ExecutionMode::Stdin);
    }

    #[test]
    fn test_toml_deserialization_detailed() {
        let toml = r#"
[runtimes.go]
command = "go run"
execution_mode = "file"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.get_runtime("go"), Some("go run"));
        assert_eq!(config.get_execution_mode("go"), ExecutionMode::File);
    }

    #[test]
    fn test_toml_deserialization_mixed() {
        let toml = r#"
[runtimes]
python = "python3"

[runtimes.go]
command = "go run"
execution_mode = "file"

[runtimes.mq]
command = "mq"
execution_mode = "arg"
"#;
        let config: Config = toml::from_str(toml).unwrap();

        // Simple config
        assert_eq!(config.get_runtime("python"), Some("python3"));
        assert_eq!(config.get_execution_mode("python"), ExecutionMode::Stdin);

        // Detailed config with file mode
        assert_eq!(config.get_runtime("go"), Some("go run"));
        assert_eq!(config.get_execution_mode("go"), ExecutionMode::File);

        // Detailed config with arg mode
        assert_eq!(config.get_runtime("mq"), Some("mq"));
        assert_eq!(config.get_execution_mode("mq"), ExecutionMode::Arg);
    }
}
