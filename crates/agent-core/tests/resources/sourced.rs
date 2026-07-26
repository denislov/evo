//! Source-aware resource loading behavior.

use agent_core::api::resources::{
    SourceTag, SourcedPromptTemplate, SourcedSkill, load_sourced_prompt_templates,
    load_sourced_skills,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn make_skill_dir(root: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {}\ndescription: a skill\n---\n\n{}", name, body),
    )
    .unwrap();
    dir
}

#[test]
fn load_sourced_skills_attaches_source_metadata() {
    let project = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    make_skill_dir(project.path(), "rustfmt", "from project");
    make_skill_dir(user.path(), "code-review", "from user");

    let inputs = vec![
        (
            project.path().to_path_buf(),
            SourceTag {
                source_path: project.path().to_path_buf(),
                source_type: "project".into(),
            },
        ),
        (
            user.path().to_path_buf(),
            SourceTag {
                source_path: user.path().to_path_buf(),
                source_type: "user".into(),
            },
        ),
    ];

    let (skills, diags) = load_sourced_skills(&inputs);
    assert!(diags.is_empty());

    let names: Vec<&str> = skills.iter().map(|s| s.skill.name.as_str()).collect();
    assert!(names.contains(&"rustfmt"));
    assert!(names.contains(&"code-review"));

    let project_skill: &SourcedSkill = skills.iter().find(|s| s.skill.name == "rustfmt").unwrap();
    assert_eq!(project_skill.source.source_type, "project");
    assert_eq!(project_skill.source.source_path, project.path());
}

#[test]
fn load_sourced_prompt_templates_attaches_source_metadata() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("review.md"),
        "---\ndescription: review changes\n---\n\nPlease review: $1",
    )
    .unwrap();

    let inputs = vec![(
        dir.path().to_path_buf(),
        SourceTag {
            source_path: dir.path().to_path_buf(),
            source_type: "builtin".into(),
        },
    )];
    let (templates, diags) = load_sourced_prompt_templates(&inputs);
    assert!(diags.is_empty());
    assert_eq!(templates.len(), 1);
    let entry: &SourcedPromptTemplate = &templates[0];
    assert_eq!(entry.template.name, "review");
    assert_eq!(entry.source.source_type, "builtin");
    assert_eq!(entry.source.source_path, dir.path());
}

#[test]
fn load_sourced_skills_diagnostics_carry_source() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("broken")).unwrap();
    std::fs::write(
        dir.path().join("broken/SKILL.md"),
        "---\nname: broken\nbroken_yaml: [unclosed\n---\nbody",
    )
    .unwrap();

    let tag = SourceTag {
        source_path: dir.path().to_path_buf(),
        source_type: "project".into(),
    };
    let inputs = vec![(dir.path().to_path_buf(), tag.clone())];
    let (_skills, diags) = load_sourced_skills(&inputs);
    assert!(!diags.is_empty());
    for d in &diags {
        assert_eq!(d.source.source_type, "project");
    }
}
