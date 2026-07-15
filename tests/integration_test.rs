use mq_task::{Config, Runner};
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

    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0], "Test Document: ");
    assert_eq!(tasks[1], "Task 1: ");
    assert_eq!(tasks[2], "Task 2: ");

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

    assert_eq!(sections.len(), 3);
    assert_eq!(sections[0].title, "Test Document");
    assert_eq!(sections[1].title, "Build");
    assert_eq!(sections[2].title, "Test");
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
fn test_env_override_and_working_dir() {
    let temp_dir = std::env::temp_dir();
    let work_dir = temp_dir.join("mq_task_test_env_and_dir");
    fs::create_dir_all(&work_dir).unwrap();
    let output_file = work_dir.join("output.txt");

    let config = Config::default();
    let mut runner = Runner::new(config);
    runner.set_env_overrides(vec![("MX_TEST_VAR".to_string(), "hello".to_string())]);
    runner.set_working_dir(Some(work_dir.clone()));

    let code = format!(
        r#"echo "$MX_TEST_VAR" > "{}"; pwd >> "{}""#,
        output_file.display(),
        output_file.display()
    );
    runner.execute_code("bash", &code).unwrap();

    let contents = fs::read_to_string(&output_file).unwrap();
    let mut lines = contents.lines();
    assert_eq!(lines.next(), Some("hello"));

    let reported_pwd = lines.next().unwrap();
    let expected_pwd = fs::canonicalize(&work_dir).unwrap();
    assert_eq!(fs::canonicalize(reported_pwd).unwrap(), expected_pwd);

    fs::remove_dir_all(work_dir).unwrap();
}
