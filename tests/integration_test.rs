use mq_task::{Config, Runner, runner::CodeBlock};
use std::fs;

#[test]
fn test_list_tasks() {
    let markdown = r#"# Test Document

## Task 1

```bash
echo "hello"
```

## Task 2

```python
print("world")
```
"#;

    let temp_dir = std::env::temp_dir();
    let test_file = temp_dir.join("test_list_tasks.md");
    fs::write(&test_file, markdown).unwrap();

    let config = Config::default();
    let mut runner = Runner::new(config);

    let tasks = runner.list_tasks(&test_file).unwrap();

    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0], "Task 1: ");
    assert_eq!(tasks[1], "Task 2: ");

    fs::remove_file(test_file).unwrap();
}

#[test]
fn test_extract_sections() {
    let markdown = r#"# Test Document

## Build

```bash
echo "building..."
```

## Test

```bash
echo "testing..."
```
"#;

    let config = Config::default();
    let mut runner = Runner::new(config);

    let sections = runner.extract_sections(markdown).unwrap();

    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].title, "Build");
    assert_eq!(sections[1].title, "Test");
}

#[test]
fn test_execute_bash() {
    let config = Config::default();
    let runner = Runner::new(config);

    let code = r#"echo "hello from bash""#;
    // Output is displayed in real-time, so we just check that execution succeeds
    runner.execute_code("bash", code).unwrap();
}

#[test]
fn test_custom_heading_level() {
    let markdown = r#"# Title

### Task 1

```bash
echo "hello"
```

### Task 2

```python
print("world")
```
"#;

    let config = mq_task::Config {
        ..Default::default()
    };
    let mut runner = Runner::new(config);
    let sections = runner.extract_sections(markdown).unwrap();

    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].title, "Task 1");
    assert_eq!(sections[1].title, "Task 2");
    assert_eq!(
        sections[1].codes[0],
        CodeBlock {
            lang: "python".to_string(),
            code: "print(\"world\")".to_string()
        }
    );
}
