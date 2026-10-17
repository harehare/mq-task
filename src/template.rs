//! The starter task file written by `mq-task init`.

/// A small, working task file that shows the main ideas; it passes
/// `mq-task check`.
pub const TASKS_TEMPLATE: &str = r#"# Tasks

Run `mq-task` to list the tasks below and `mq-task <task>` to run one.
Every heading is a task, and the code blocks under it are what runs.

## hello

Greet someone. Pass a name with `mq-task hello -- Ada`.

```meta
params = ["name=world"]
```

```bash
echo "Hello, $MX_PARAM_NAME!"
```

## build

Replace this with your build command.

```bash
echo "building..."
```

## test

Run the tests after a build.

```meta
depends = ["build"]
```

```bash
echo "testing..."
```

## ci

Run everything. `parallel = true` would run the dependencies together.

```meta
depends = ["build", "test"]
```

```bash
echo "all done"
```
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::check_sections;
    use crate::{Config, Runner};

    #[test]
    fn the_template_has_the_documented_tasks_and_passes_check() {
        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(TASKS_TEMPLATE).unwrap();
        let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Tasks", "hello", "build", "test", "ci"]);

        let found: Vec<_> = check_sections(&sections, &[], &Config::default(), &[])
            .into_iter()
            // `bash` may be missing on an unusual machine.
            .filter(|d| !d.message.contains("not found in PATH"))
            .collect();
        assert!(found.is_empty(), "{found:?}");
    }
}
