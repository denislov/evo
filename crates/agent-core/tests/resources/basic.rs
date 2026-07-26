//! Basic skill/template parsing and loading behavior.

use agent_core::api::resources::{
    ResourceLoadError, ResourceLoadLimit, ResourceLoadPolicy, load_prompt_templates,
    load_prompt_templates_async, load_prompt_templates_with_policy, load_skills,
    load_skills_with_policy,
};
use std::io::Write;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[test]
fn load_skills_from_directory() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("rust");
    std::fs::create_dir(&skill_dir).unwrap();

    let mut f = std::fs::File::create(skill_dir.join("SKILL.md")).unwrap();
    writeln!(
        f,
        "---\nname: rust\ndescription: Rust programming\n---\n\nRust programming guide content."
    )
    .unwrap();

    let (skills, diags) = load_skills(&[skill_dir]);
    assert!(diags.is_empty(), "diags: {:?}", diags);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "rust");
    assert_eq!(skills[0].description, "Rust programming");
    assert!(!skills[0].disable_model_invocation);
}

#[test]
fn ignored_directories_skipped() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("visible");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: visible\ndescription: A visible skill\n---\n\ncontent",
    )
    .unwrap();

    let hidden_dir = dir.path().join("hidden");
    std::fs::create_dir(&hidden_dir).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "hidden/\n").unwrap();

    let (skills, _) = load_skills(&[dir.path().to_path_buf()]);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "visible");
}

#[test]
fn load_prompt_templates_from_files() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("review.md"),
        "---\ndescription: Review changes\n---\n\nPlease review $1 and $2.",
    )
    .unwrap();

    let (templates, diags) = load_prompt_templates(&[dir.path().join("review.md")]);
    assert!(diags.is_empty());
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].name, "review");
    assert_eq!(templates[0].description, "Review changes");
}

#[test]
fn loads_from_directory_sorted() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("b.md"), "content b").unwrap();
    std::fs::write(dir.path().join("a.md"), "content a").unwrap();
    std::fs::write(dir.path().join("c.txt"), "not md").unwrap();

    let (templates, _) = load_prompt_templates(&[dir.path().to_path_buf()]);
    assert_eq!(templates.len(), 2);
    assert_eq!(templates[0].name, "a");
    assert_eq!(templates[1].name, "b");
}

#[test]
fn resource_file_and_aggregate_byte_limits_fail_closed() {
    let dir = TempDir::new().unwrap();
    let first = dir.path().join("first.md");
    let second = dir.path().join("second.md");
    std::fs::write(&first, "x".repeat(65)).unwrap();
    std::fs::write(&second, "y".repeat(40)).unwrap();

    let file_error = load_prompt_templates_with_policy(
        std::slice::from_ref(&first),
        ResourceLoadPolicy {
            max_file_bytes: 64,
            max_total_bytes: 128,
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(matches!(
        file_error,
        ResourceLoadError::Limit {
            limit: ResourceLoadLimit::FileBytes,
            max: 64,
            ..
        }
    ));

    std::fs::write(&first, "x".repeat(40)).unwrap();
    let total_error = load_prompt_templates_with_policy(
        &[first, second],
        ResourceLoadPolicy {
            max_file_bytes: 64,
            max_total_bytes: 64,
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(matches!(
        total_error,
        ResourceLoadError::Limit {
            limit: ResourceLoadLimit::TotalBytes,
            max: 64,
            ..
        }
    ));
}

#[test]
fn resource_count_and_depth_limits_bound_tree_traversal() {
    let dir = TempDir::new().unwrap();
    for name in ["a.md", "b.md", "c.md"] {
        std::fs::write(dir.path().join(name), "template").unwrap();
    }
    let count_error = load_prompt_templates_with_policy(
        &[dir.path().to_path_buf()],
        ResourceLoadPolicy {
            max_files: 2,
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(matches!(
        count_error,
        ResourceLoadError::Limit {
            limit: ResourceLoadLimit::Files,
            max: 2,
            ..
        }
    ));

    let deep = dir.path().join("one").join("two").join("three");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(
        deep.join("SKILL.md"),
        "---\nname: three\ndescription: too deep\n---\ncontent",
    )
    .unwrap();
    let (skills, _) = load_skills_with_policy(
        &[dir.path().to_path_buf()],
        ResourceLoadPolicy {
            max_depth: 2,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert!(
        skills.is_empty(),
        "a SKILL.md below the traversal depth must not be loaded"
    );
}

#[cfg(unix)]
#[test]
fn resource_symlinks_are_rejected_without_following_targets() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let outside = dir.path().join("outside.md");
    std::fs::write(
        &outside,
        "---\nname: outside\ndescription: outside\n---\nsecret",
    )
    .unwrap();
    let link = dir.path().join("linked.md");
    symlink(&outside, &link).unwrap();

    let (skills, diagnostics) = load_skills(&[link]);
    assert!(skills.is_empty());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "resource_symlink_rejected")
    );
}

#[tokio::test]
async fn async_resource_loader_observes_cancellation_before_blocking_work() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result =
        load_prompt_templates_async(Vec::new(), ResourceLoadPolicy::default(), cancellation).await;
    assert!(matches!(result, Err(ResourceLoadError::Cancelled)));
}
