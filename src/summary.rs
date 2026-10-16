//! Human-readable description of a single task (`--summary`).

use std::fmt::Write;

use crate::graph::badge_list;
use crate::runner::Section;

/// Describe `section`: title, description and its notable settings.
pub fn render(section: &Section) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", section.title);
    if let Some(desc) = section.description.as_deref().map(str::trim)
        && !desc.is_empty()
    {
        for line in desc.lines() {
            let _ = writeln!(out, "  {}", line.trim());
        }
    }

    let mut rows: Vec<(&str, String)> = Vec::new();
    if !section.aliases.is_empty() {
        rows.push(("aliases", section.aliases.join(", ")));
    }
    if !section.depends.is_empty() {
        rows.push(("depends", section.depends.join(", ")));
    }
    if !section.params.is_empty() {
        let params: Vec<String> = section
            .params
            .iter()
            .map(|p| match &p.default {
                Some(default) => format!("{}={}", p.name, default),
                None => format!("{} (required)", p.name),
            })
            .collect();
        rows.push(("params", params.join(", ")));
    }
    if !section.env.is_empty() {
        let env: Vec<String> = section
            .env
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        rows.push(("env", env.join(", ")));
    }
    if let Some(dir) = &section.dir {
        rows.push(("dir", dir.display().to_string()));
    }
    if !section.dotenv.is_empty() {
        rows.push(("dotenv", section.dotenv.join(", ")));
    }
    let badges = badge_list(section);
    if !badges.is_empty() {
        rows.push(("options", badges.join(", ")));
    }
    if let Some(prompt) = &section.prompt {
        rows.push(("prompt", prompt.clone()));
    }
    if !section.sources.is_empty() {
        rows.push(("sources", section.sources.join(", ")));
    }
    if !section.outputs.is_empty() {
        rows.push(("outputs", section.outputs.join(", ")));
    }
    if !section.codes.is_empty() {
        let mut langs: Vec<&str> = Vec::new();
        for code in &section.codes {
            if !langs.contains(&code.lang.as_str()) {
                langs.push(&code.lang);
            }
        }
        rows.push((
            "code",
            format!("{} block(s): {}", section.codes.len(), langs.join(", ")),
        ));
    }

    if !rows.is_empty() {
        out.push('\n');
    }
    for (label, value) in rows {
        let _ = writeln!(out, "  {:<8} {}", label, value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{CodeBlock, ParamDef};

    #[test]
    fn describes_a_task() {
        let section = Section {
            title: "deploy".into(),
            description: Some("Deploy the app.".into()),
            aliases: vec!["d".into()],
            depends: vec!["build".into()],
            params: vec![
                ParamDef {
                    name: "env".into(),
                    default: Some("staging".into()),
                },
                ParamDef {
                    name: "region".into(),
                    default: None,
                },
            ],
            retries: 2,
            codes: vec![
                CodeBlock {
                    lang: "bash".into(),
                    code: "true".into(),
                },
                CodeBlock {
                    lang: "bash".into(),
                    code: "true".into(),
                },
            ],
            ..Default::default()
        };
        let text = render(&section);
        assert!(
            text.starts_with("deploy\n  Deploy the app.\n\n"),
            "{}",
            text
        );
        assert!(text.contains("  aliases  d\n"));
        assert!(text.contains("  depends  build\n"));
        assert!(text.contains("  params   env=staging, region (required)\n"));
        assert!(text.contains("  options  retries: 2\n"));
        assert!(text.contains("  code     2 block(s): bash\n"));
    }

    #[test]
    fn a_bare_task_is_just_its_title() {
        let section = Section {
            title: "t".into(),
            ..Default::default()
        };
        assert_eq!(render(&section), "t\n");
    }
}
