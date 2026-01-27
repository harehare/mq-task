use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use mq_lang::{Engine, Ident, RuntimeValue, parse_markdown_input};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ExecutionMode};
use crate::error::{Error, Result};

const SECTIONS_QUERY: &str = include_str!("../sections.mq");

/// Represents a code block in a section
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeBlock {
    /// Language of the code block
    pub lang: String,
    /// Code content
    pub code: String,
}

/// Represents a section with its code blocks
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Section {
    /// Section title
    pub title: String,
    /// Code blocks in this section
    pub codes: Vec<CodeBlock>,
    /// Optional description extracted from the section content
    pub description: Option<String>,
}

/// Task runner that executes code blocks in Markdown sections
pub struct Runner {
    config: Config,
    engine: Engine,
}

impl Runner {
    /// Create a new Runner with the given configuration
    pub fn new(config: Config) -> Self {
        let mut engine: Engine = Engine::default();
        engine.load_builtin_module();

        Self { config, engine }
    }

    /// Create a new Runner with default configuration
    pub fn with_default_config() -> Self {
        Self::new(Config::default())
    }

    /// Load and parse a Markdown file
    pub fn load_markdown<P: AsRef<Path>>(&self, path: P) -> Result<String> {
        fs::read_to_string(path).map_err(Error::Io)
    }

    /// Extract sections from Markdown content
    pub fn extract_sections(&mut self, markdown: &str) -> Result<Vec<Section>> {
        let input = parse_markdown_input(markdown)
            .map_err(|e| Error::Markdown(format!("Failed to parse markdown: {}", e)))?;

        let query = format!("{}\n | nodes | sections_with_code()", SECTIONS_QUERY);
        let result = self
            .engine
            .eval(&query, input.into_iter())
            .map_err(|e| Error::Query(format!("Failed to execute query: {}", e)))?;

        let sections = self.parse_sections(result)?;

        Ok(sections)
    }

    fn parse_sections(&self, result: mq_lang::RuntimeValues) -> Result<Vec<Section>> {
        let mut sections = Vec::new();

        for value in result.into_iter() {
            if let RuntimeValue::Dict(dict) = value {
                let section = self.parse_section(&dict)?;
                sections.push(section);
            }
        }

        Ok(sections)
    }

    fn parse_section(&self, dict: &BTreeMap<Ident, RuntimeValue>) -> Result<Section> {
        let title = dict
            .get(&Ident::from("title"))
            .and_then(|v| match v {
                RuntimeValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_default();

        let codes = dict
            .get(&Ident::from("codes"))
            .and_then(|v| match v {
                RuntimeValue::Array(arr) => Some(self.parse_code_blocks(arr)),
                _ => None,
            })
            .unwrap_or_else(|| Ok(Vec::new()))?;

        let description = dict.get(&Ident::from("description")).and_then(|v| match v {
            RuntimeValue::String(s) => Some(s.to_string()),
            _ => None,
        });

        Ok(Section {
            title,
            codes,
            description,
        })
    }

    fn parse_code_blocks(&self, arr: &[RuntimeValue]) -> Result<Vec<CodeBlock>> {
        let mut blocks = Vec::new();

        for item in arr {
            if let RuntimeValue::Dict(dict) = item {
                let lang = dict
                    .get(&Ident::from("lang"))
                    .and_then(|v| match v {
                        RuntimeValue::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let code = dict
                    .get(&Ident::from("code"))
                    .and_then(|v| match v {
                        RuntimeValue::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();

                blocks.push(CodeBlock { lang, code });
            }
        }

        Ok(blocks)
    }

    pub fn find_section<'a>(&self, sections: &'a [Section], title: &str) -> Option<&'a Section> {
        sections.iter().find(|s| s.title == title)
    }

    pub fn execute_section(&self, section: &Section) -> Result<()> {
        self.execute_section_with_args(section, &[])
    }

    pub fn execute_section_with_args(&self, section: &Section, args: &[String]) -> Result<()> {
        self.execute_section_with_lang_filter(section, args, None)
    }

    pub fn execute_section_with_lang_filter(
        &self,
        section: &Section,
        args: &[String],
        lang_filter: Option<&str>,
    ) -> Result<()> {
        for code_block in &section.codes {
            if code_block.lang.is_empty() {
                continue;
            }

            // Apply language filter if specified
            if let Some(filter) = lang_filter
                && code_block.lang != filter
            {
                continue;
            }

            self.execute_code_with_args(&code_block.lang, &code_block.code, args)?;
        }

        Ok(())
    }

    pub fn execute_code(&self, lang: &str, code: &str) -> Result<()> {
        self.execute_code_with_args(lang, code, &[])
    }

    pub fn execute_code_with_args(&self, lang: &str, code: &str, args: &[String]) -> Result<()> {
        let runtime = self
            .config
            .get_runtime(lang)
            .ok_or_else(|| Error::RuntimeNotFound(lang.to_string()))?;

        let parts: Vec<&str> = runtime.split_whitespace().collect();
        if parts.is_empty() {
            return Err(Error::RuntimeNotFound(lang.to_string()));
        }

        // Get execution mode from config
        let execution_mode = self.config.get_execution_mode(lang);

        match execution_mode {
            ExecutionMode::File => self.execute_code_with_file_and_args(lang, code, &parts, args),
            ExecutionMode::Arg => self.execute_code_with_arg_mode(code, &parts, args),
            ExecutionMode::Stdin => self.execute_code_with_stdin_and_args(code, &parts, args),
        }
    }

    fn execute_code_with_stdin_and_args(
        &self,
        code: &str,
        parts: &[&str],
        task_args: &[String],
    ) -> Result<()> {
        let cmd = parts[0];
        let args = &parts[1..];

        // Use inherit() for stdout/stderr to preserve TTY and colors
        let mut child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .envs(Self::prepare_env_vars(task_args))
            .spawn()
            .map_err(|e| Error::Execution(format!("Failed to spawn process: {}", e)))?;

        // Write code to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(code.as_bytes())
                .map_err(|e| Error::Execution(format!("Failed to write to stdin: {}", e)))?;
            drop(stdin);
        }

        // Wait for completion
        let status = child
            .wait()
            .map_err(|e| Error::Execution(format!("Failed to wait for process: {}", e)))?;

        if !status.success() {
            return Err(Error::Execution("Execution failed".to_string()));
        }

        Ok(())
    }

    fn execute_code_with_arg_mode(
        &self,
        code: &str,
        parts: &[&str],
        task_args: &[String],
    ) -> Result<()> {
        let cmd = parts[0];
        // Append code as an argument to the command
        let mut args: Vec<&str> = parts[1..].to_vec();
        args.push(code);

        // Use inherit() for stdout/stderr to preserve TTY and colors
        let mut child = Command::new(cmd)
            .args(args)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .envs(Self::prepare_env_vars(task_args))
            .spawn()
            .map_err(|e| Error::Execution(format!("Failed to spawn process: {}", e)))?;

        // Write code to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(code.as_bytes())
                .map_err(|e| Error::Execution(format!("Failed to write to stdin: {}", e)))?;
            drop(stdin);
        }

        // Wait for completion
        let status = child
            .wait()
            .map_err(|e| Error::Execution(format!("Failed to wait for process: {}", e)))?;

        if !status.success() {
            return Err(Error::Execution("Execution failed".to_string()));
        }

        Ok(())
    }

    fn execute_code_with_file_and_args(
        &self,
        lang: &str,
        code: &str,
        parts: &[&str],
        task_args: &[String],
    ) -> Result<()> {
        use std::env;

        // Create temporary directory
        let temp_dir = env::temp_dir();

        // Use language name as file extension, or map known languages
        let file_ext = match lang {
            "go" | "golang" => "go",
            "python" => "py",
            "ruby" => "rb",
            "javascript" | "js" => "js",
            "typescript" | "ts" => "ts",
            _ => lang, // Use language name as extension for custom languages
        };

        // Generate unique file name
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_name = format!("mx_temp_{}.{}", timestamp, file_ext);
        let temp_file = temp_dir.join(&file_name);

        // Write code to temporary file
        fs::write(&temp_file, code)
            .map_err(|e| Error::Execution(format!("Failed to write temp file: {}", e)))?;

        // Execute go run <file>
        let status = Command::new(parts[0])
            .args(&parts[1..])
            .arg(&temp_file)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .envs(Self::prepare_env_vars(task_args))
            .status()
            .map_err(|e| Error::Execution(format!("Failed to execute {}: {}", lang, e)))?;

        // Clean up temporary file
        fs::remove_file(&temp_file).ok();

        if !status.success() {
            Err(Error::Execution(format!("{} execution failed", lang)))
        } else {
            Ok(())
        }
    }

    /// Prepare environment variables from task arguments
    fn prepare_env_vars(args: &[String]) -> Vec<(String, String)> {
        let mut env_vars = Vec::new();

        // Set MX_ARGS with all arguments joined by space
        if !args.is_empty() {
            env_vars.push(("MX_ARGS".to_string(), args.join(" ")));
        }

        // Set individual arguments as MX_ARG_0, MX_ARG_1, etc.
        for (i, arg) in args.iter().enumerate() {
            env_vars.push((format!("MX_ARG_{}", i), arg.clone()));
        }

        env_vars
    }

    /// Run a specific task by section title
    pub fn run_task<P: AsRef<Path>>(&mut self, markdown_path: P, task_name: &str) -> Result<()> {
        self.run_task_with_args(markdown_path, task_name, &[])
    }

    /// Run a specific task with arguments
    pub fn run_task_with_args<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        task_name: &str,
        args: &[String],
    ) -> Result<()> {
        self.run_task_with_lang_filter(markdown_path, task_name, args, None)
    }

    /// Run a specific task with arguments and language filter
    pub fn run_task_with_lang_filter<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        task_name: &str,
        args: &[String],
        lang_filter: Option<&str>,
    ) -> Result<()> {
        let markdown = self.load_markdown(markdown_path)?;
        let sections = self.extract_sections(&markdown)?;

        let section = self
            .find_section(&sections, task_name)
            .ok_or_else(|| Error::SectionNotFound(task_name.to_string()))?;

        self.execute_section_with_lang_filter(section, args, lang_filter)
    }

    /// List all available tasks (sections) in a Markdown file
    pub fn list_tasks<P: AsRef<Path>>(&mut self, markdown_path: P) -> Result<Vec<String>> {
        let markdown = self.load_markdown(markdown_path)?;
        let sections = self.extract_sections(&markdown)?;

        Ok(sections
            .into_iter()
            .map(|s| format!("{}: {}", s.title, s.description.unwrap_or_default()))
            .collect())
    }

    /// List all available task sections in a Markdown file with their details
    pub fn list_task_sections<P: AsRef<Path>>(&mut self, markdown_path: P) -> Result<Vec<Section>> {
        let markdown = self.load_markdown(markdown_path)?;
        self.extract_sections(&markdown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_sections() {
        let markdown = r#"# Title

## Task 1

```bash
echo "hello"
```

## Task 2

```python
print("world")
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();

        assert_eq!(sections.len(), 3);
        assert_eq!(sections[1].title, "Task 1");
        assert_eq!(sections[1].codes.len(), 1);
        assert_eq!(sections[1].codes[0].lang, "bash");
    }

    #[test]
    fn test_find_section() {
        let sections = vec![
            Section {
                title: "Task 1".to_string(),
                ..Default::default()
            },
            Section {
                title: "Task 2".to_string(),
                ..Default::default()
            },
        ];

        let runner = Runner::with_default_config();
        let found = runner.find_section(&sections, "Task 1");
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "Task 1");

        let not_found = runner.find_section(&sections, "Task 3");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_language_filter() {
        let section = Section {
            title: "Mixed Task".to_string(),
            codes: vec![
                CodeBlock {
                    lang: "bash".to_string(),
                    code: "echo 'bash code'".to_string(),
                },
                CodeBlock {
                    lang: "python".to_string(),
                    code: "print('python code')".to_string(),
                },
                CodeBlock {
                    lang: "bash".to_string(),
                    code: "echo 'more bash'".to_string(),
                },
            ],
            description: None,
        };

        let runner = Runner::with_default_config();

        // Test filtering for bash only - this will fail if bash is not available,
        // but demonstrates the filtering logic
        let result = runner.execute_section_with_lang_filter(&section, &[], Some("bash"));
        // We can't guarantee bash is available in test environment, so we just check
        // that the method runs without panicking
        let _ = result;
    }

    #[test]
    fn test_extract_sections_with_multiple_languages() {
        let markdown = r#"# Title

## Mixed Task

```bash
echo "bash code"
```

```python
print("python code")
```

```bash
echo "more bash"
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();

        assert_eq!(sections.len(), 2);
        assert_eq!(sections[1].title, "Mixed Task");
        assert_eq!(sections[1].codes.len(), 3);
        assert_eq!(sections[1].codes[0].lang, "bash");
        assert_eq!(sections[1].codes[1].lang, "python");
        assert_eq!(sections[1].codes[2].lang, "bash");
    }
}
