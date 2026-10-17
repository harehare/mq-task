//! Human-readable description of a single task (`--summary`).

use std::fmt::Write;

use crate::graph::badge_list;
use crate::runner::Section;
use crate::vars::VarValue;

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
    if !section.vars.is_empty() {
        let vars: Vec<String> = section
            .vars
            .iter()
            .map(|v| match v.value {
                VarValue::Literal(_) => v.name.clone(),
                VarValue::Shell(_) => format!("{} (sh)", v.name),
            })
            .collect();
        rows.push(("vars", vars.join(", ")));
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

/// The tasks as a JSON array, for scripts and editors (`list --json`).
pub fn list_json(sections: &[Section]) -> String {
    let tasks: Vec<serde_json::Value> = sections
        .iter()
        .map(|s| {
            let mut languages: Vec<&str> = Vec::new();
            for code in &s.codes {
                if !languages.contains(&code.lang.as_str()) {
                    languages.push(&code.lang);
                }
            }
            serde_json::json!({
                "name": s.title,
                "description": s.description.as_deref().map(str::trim).filter(|d| !d.is_empty()),
                "aliases": s.aliases,
                "depends": s.depends,
                "params": s.params.iter().map(|p| serde_json::json!({
                    "name": p.name,
                    "default": p.default,
                    "required": p.default.is_none(),
                })).collect::<Vec<_>>(),
                "private": s.private,
                "languages": languages,
            })
        })
        .collect();
    serde_json::to_string_pretty(&tasks).unwrap_or_else(|_| "[]".to_string())
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

    #[test]
    fn lists_tasks_as_json() {
        let sections = vec![Section {
            title: "build".into(),
            description: Some("  Build it. ".into()),
            aliases: vec!["b".into()],
            params: vec![ParamDef {
                name: "profile".into(),
                default: Some("dev".into()),
            }],
            codes: vec![CodeBlock {
                lang: "bash".into(),
                code: "true".into(),
            }],
            ..Default::default()
        }];
        let value: serde_json::Value = serde_json::from_str(&list_json(&sections)).unwrap();
        let task = &value[0];
        assert_eq!(task["name"], "build");
        assert_eq!(task["description"], "Build it.");
        assert_eq!(task["aliases"][0], "b");
        assert_eq!(task["params"][0]["default"], "dev");
        assert_eq!(task["params"][0]["required"], false);
        assert_eq!(task["languages"][0], "bash");
        assert_eq!(task["private"], false);
        assert_eq!(list_json(&[]), "[]");
    }

    #[test]
    fn lists_every_setting_that_is_present() {
        let section = Section {
            title: "t".into(),
            env: vec![("A".into(), "1".into()), ("B".into(), "2".into())],
            vars: vec![
                crate::vars::VarDef {
                    name: "NAME".into(),
                    value: VarValue::Literal("app".into()),
                },
                crate::vars::VarDef {
                    name: "VERSION".into(),
                    value: VarValue::Shell("git describe".into()),
                },
            ],
            dir: Some("svc".into()),
            dotenv: vec![".env".into()],
            prompt: Some("sure?".into()),
            sources: vec!["src/**".into()],
            outputs: vec!["out".into()],
            ..Default::default()
        };
        let text = render(&section);
        for row in [
            "  env      A=1, B=2\n",
            "  vars     NAME, VERSION (sh)\n",
            "  dir      svc\n",
            "  dotenv   .env\n",
            "  options  prompt\n",
            "  prompt   sure?\n",
            "  sources  src/**\n",
            "  outputs  out\n",
        ] {
            assert!(text.contains(row), "{}", text);
        }
    }

    #[test]
    fn multi_line_descriptions_are_indented_and_blank_ones_dropped() {
        let section = Section {
            title: "t".into(),
            description: Some("first\n second ".into()),
            ..Default::default()
        };
        assert_eq!(render(&section), "t\n  first\n  second\n");
        let blank = Section {
            title: "t".into(),
            description: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(render(&blank), "t\n");
    }

    #[test]
    fn json_marks_private_tasks_and_missing_descriptions() {
        let sections = vec![Section {
            title: "_hidden".into(),
            private: true,
            params: vec![ParamDef {
                name: "x".into(),
                default: None,
            }],
            ..Default::default()
        }];
        let value: serde_json::Value = serde_json::from_str(&list_json(&sections)).unwrap();
        assert_eq!(value[0]["private"], true);
        assert!(value[0]["description"].is_null());
        assert_eq!(value[0]["params"][0]["required"], true);
    }
}
