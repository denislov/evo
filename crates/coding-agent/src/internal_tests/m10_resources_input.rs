//! Internal owner tests spanning resources, input, tools, and the interactive harness.

use super::support::ProviderGuard;
use crate::internal_tests::product_fixture::command::{
    PromptRuntimeOptions, run_prompt_text_for_tests,
};
use crate::internal_tests::product_fixture::configuration::{parse_model_rotation, select_model};
use crate::internal_tests::product_fixture::input::{
    ImageProcessingOptions, ImageResizeOptions, merge_stdin_prompt, process_at_file_references,
    process_at_file_references_with_options, process_at_file_references_with_processing_options,
};
use crate::internal_tests::product_fixture::resources::{
    ResourceLoadOptions, ToolFilter, builtin_tools, discover_context_files, filter_tools,
    load_application_resources_with_options, load_themes,
};
use agent_core::api::agent::ThinkingLevel;
use ai::api::conversation::{AssistantMessage, ContentBlock, Context, Message, StopReason};
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::provider::ApiProvider;
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use coding_agent::api::embedding::CodingAgentInvocationOptions;
use coding_agent::api::operation::PromptInvocation;
use std::sync::{Arc, Mutex};

#[test]
fn discovers_global_and_ancestor_context_files_in_priority_order() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let root = temp.path().join("repo");
    let child = root.join("crates/app");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(global.join("AGENTS.md"), "global").unwrap();
    std::fs::write(root.join("AGENTS.md"), "root").unwrap();
    std::fs::write(root.join("crates").join("CLAUDE.md"), "crates").unwrap();
    std::fs::write(child.join("AGENTS.md"), "child").unwrap();

    let files = discover_context_files(&child, &global, false);
    let contents: Vec<_> = files.iter().map(|file| file.content.as_str()).collect();
    assert_eq!(contents, vec!["global", "root", "crates", "child"]);

    assert!(discover_context_files(&child, &global, true).is_empty());
}

#[test]
fn resource_loader_uses_default_paths_and_disable_switches() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    let agent_dir = temp.path().join("agent");
    let skill_dir = agent_dir.join("skills/rust");
    let prompt_dir = agent_dir.join("prompt-templates");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::create_dir_all(&prompt_dir).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: rust\ndescription: Rust help\n---\nUse Rust.",
    )
    .unwrap();
    std::fs::write(
        prompt_dir.join("review.md"),
        "---\nname: review\ndescription: Review\n---\nReview $ARGUMENTS",
    )
    .unwrap();

    let loaded = load_application_resources_with_options(
        &[],
        &[],
        &cwd,
        &agent_dir,
        ResourceLoadOptions::default(),
    )
    .unwrap();
    assert_eq!(loaded.skills[0].name, "rust");
    assert_eq!(loaded.prompt_templates[0].name, "review");

    let loaded = load_application_resources_with_options(
        &[],
        &[],
        &cwd,
        &agent_dir,
        ResourceLoadOptions {
            no_skills: true,
            no_prompt_templates: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(loaded.skills.is_empty());
    assert!(loaded.prompt_templates.is_empty());
}

#[test]
fn resource_loader_includes_settings_paths_and_prompt_alias_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    let agent_dir = temp.path().join("agent");
    let settings_skill_dir = temp.path().join("settings-skills").join("go");
    let settings_prompt_dir = temp.path().join("settings-prompts");
    let default_prompt_alias_dir = agent_dir.join("prompts");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&settings_skill_dir).unwrap();
    std::fs::create_dir_all(&settings_prompt_dir).unwrap();
    std::fs::create_dir_all(&default_prompt_alias_dir).unwrap();
    std::fs::write(
        settings_skill_dir.join("SKILL.md"),
        "---\nname: go\ndescription: Go help\n---\nUse Go.",
    )
    .unwrap();
    std::fs::write(
        settings_prompt_dir.join("summarize.md"),
        "---\nname: summarize\ndescription: Summarize\n---\nSummarize $ARGUMENTS",
    )
    .unwrap();
    std::fs::write(
        default_prompt_alias_dir.join("alias.md"),
        "---\nname: alias\ndescription: Alias\n---\nAlias $ARGUMENTS",
    )
    .unwrap();

    let loaded = load_application_resources_with_options(
        &[],
        &[],
        &cwd,
        &agent_dir,
        ResourceLoadOptions {
            skill_paths: vec![temp.path().join("settings-skills").display().to_string()],
            prompt_paths: vec![settings_prompt_dir.display().to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    assert!(loaded.skills.iter().any(|skill| skill.name == "go"));
    let template_names: Vec<_> = loaded
        .prompt_templates
        .iter()
        .map(|template| template.name.as_str())
        .collect();
    assert!(template_names.contains(&"summarize"));
    assert!(template_names.contains(&"alias"));
}

#[test]
fn resource_loader_discovers_themes_and_honors_no_themes() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    let agent_dir = temp.path().join("agent");
    let default_theme_dir = agent_dir.join("themes");
    let settings_theme_dir = temp.path().join("settings-themes");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&default_theme_dir).unwrap();
    std::fs::create_dir_all(&settings_theme_dir).unwrap();
    std::fs::write(
        default_theme_dir.join("dark.json"),
        r##"{"name":"dark","colors":{"text":"#ffffff"}}"##,
    )
    .unwrap();
    std::fs::write(
        settings_theme_dir.join("quiet.json"),
        r##"{"name":"quiet","colors":{"text":"#eeeeee"}}"##,
    )
    .unwrap();

    let loaded = load_application_resources_with_options(
        &[],
        &[],
        &cwd,
        &agent_dir,
        ResourceLoadOptions {
            theme_paths: vec![settings_theme_dir.display().to_string()],
            theme: Some("quiet".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let (themes, _) = load_themes(&[default_theme_dir, settings_theme_dir.clone()]);
    let theme_names: Vec<_> = themes.iter().map(|theme| theme.name.as_str()).collect();
    assert!(theme_names.contains(&"dark"));
    assert!(theme_names.contains(&"quiet"));
    assert_eq!(
        loaded.selected_theme.as_ref().map(|t| t.name.as_str()),
        Some("quiet")
    );

    let loaded = load_application_resources_with_options(
        &[],
        &[],
        &cwd,
        &agent_dir,
        ResourceLoadOptions {
            no_themes: true,
            theme_paths: vec![settings_theme_dir.display().to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(loaded.selected_theme.is_none());
}

#[test]
fn at_file_references_expand_text_and_images() {
    let temp = tempfile::tempdir().unwrap();
    let text_path = temp.path().join("note.txt");
    let png_path = temp.path().join("tiny.png");
    std::fs::write(&text_path, "alpha\nbeta").unwrap();
    std::fs::write(
        &png_path,
        [
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', b'i', b'm', b'g',
        ],
    )
    .unwrap();

    let prompt = format!(
        "inspect @{} and @{}",
        text_path.display(),
        png_path.display()
    );
    let processed = process_at_file_references(&prompt, temp.path()).unwrap();

    assert!(processed.text.contains("inspect"));
    assert!(processed.text.contains("<file name="));
    assert!(processed.text.contains("alpha\nbeta"));
    assert_eq!(processed.images.len(), 1);
    assert_eq!(processed.images[0].mime_type, "image/png");
    assert!(processed.images[0].data.starts_with("iVBOR"));
    assert!(
        processed
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
    );
}

#[test]
fn image_file_references_resize_large_images() {
    let temp = tempfile::tempdir().unwrap();
    let png_path = temp.path().join("wide.png");
    let mut image = image::RgbImage::new(4, 2);
    for pixel in image.pixels_mut() {
        *pixel = image::Rgb([255, 0, 0]);
    }
    image.save(&png_path).unwrap();

    let processed = process_at_file_references_with_options(
        &format!("@{}", png_path.display()),
        temp.path(),
        ImageResizeOptions {
            max_dimension: 2,
            enabled: true,
        },
    )
    .unwrap();

    let data = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &processed.images[0].data,
    )
    .unwrap();
    let resized = image::load_from_memory(&data).unwrap();
    assert_eq!((resized.width(), resized.height()), (2, 1));
    assert!(processed.text.contains("resized from 4x2 to 2x1"));
}

#[test]
fn image_file_references_preserve_original_when_resize_is_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let png_path = temp.path().join("wide.png");
    let mut image = image::RgbImage::new(4, 2);
    for pixel in image.pixels_mut() {
        *pixel = image::Rgb([0, 255, 0]);
    }
    image.save(&png_path).unwrap();

    let processed = process_at_file_references_with_processing_options(
        &format!("@{}", png_path.display()),
        temp.path(),
        ImageProcessingOptions {
            resize: ImageResizeOptions {
                max_dimension: 2,
                enabled: false,
            },
            block_images: false,
        },
    )
    .unwrap();

    let data = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &processed.images[0].data,
    )
    .unwrap();
    let original = image::load_from_memory(&data).unwrap();
    assert_eq!((original.width(), original.height()), (4, 2));
    assert!(!processed.text.contains("resized from"));
}

#[test]
fn image_file_references_are_blocked_when_image_input_is_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let png_path = temp.path().join("blocked.png");
    std::fs::write(
        &png_path,
        [
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', b'i', b'm', b'g',
        ],
    )
    .unwrap();

    let processed = process_at_file_references_with_processing_options(
        &format!("inspect @{}", png_path.display()),
        temp.path(),
        ImageProcessingOptions {
            resize: ImageResizeOptions {
                max_dimension: 2,
                enabled: true,
            },
            block_images: true,
        },
    )
    .unwrap();

    assert!(processed.images.is_empty());
    assert!(processed.text.contains("inspect"));
    assert!(processed.text.contains("[Image reading is disabled.]"));
    assert!(
        !processed
            .content
            .iter()
            .any(|block| { matches!(block, ContentBlock::Image { .. }) })
    );
}

#[test]
fn at_file_references_support_quoted_paths_with_spaces() {
    let temp = tempfile::tempdir().unwrap();
    let text_path = temp.path().join("notes with spaces.txt");
    std::fs::write(&text_path, "spaced content").unwrap();

    let processed =
        process_at_file_references(r#"read @"notes with spaces.txt" now"#, temp.path()).unwrap();

    assert!(processed.text.contains("read"));
    assert!(processed.text.contains("spaced content"));
    assert!(processed.text.contains("now"));
}

fn faux_model(api: &str) -> Model {
    Model {
        id: "m10-faux-model".into(),
        name: "M10 Faux".into(),
        api: api.into(),
        provider: "faux".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        cost: ModelCost::default(),
        context_window: 10_000,
        max_tokens: 1_000,
        headers: None,
        compat: None,
    }
}

struct RecordingProvider {
    contexts: Arc<Mutex<Vec<Context>>>,
}

impl ApiProvider for RecordingProvider {
    fn stream(&self, model: &Model, ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        self.contexts.lock().unwrap().push(ctx);
        let model_id = model.id.clone();
        Box::pin(futures::stream::iter(vec![AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: {
                let mut message = AssistantMessage::empty("recording", &model_id);
                message.content.push(ContentBlock::Text {
                    text: "ok".into(),
                    text_signature: None,
                });
                message.stop_reason = StopReason::Stop;
                message
            },
        }]))
    }
}

#[tokio::test]
async fn multimodal_prompt_content_reaches_provider_context() {
    let api = "m10-multimodal-provider";
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(RecordingProvider {
            contexts: contexts.clone(),
        }),
    );

    let output = run_prompt_text_for_tests(PromptRuntimeOptions {
        model: faux_model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(1),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: Some(_provider_guard.ai_client()),
        session: None,
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::resources::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Content(vec![
            ContentBlock::Text {
                text: "inspect image".into(),
                text_signature: None,
            },
            ContentBlock::Image {
                data: "iVBORw0KGgo=".into(),
                mime_type: "image/png".into(),
            },
        ]),
    })
    .await
    .unwrap();

    assert_eq!(output, "ok");
    let contexts = contexts.lock().unwrap();
    let first_message = contexts[0].messages.first().expect("first message");
    let content = match first_message {
        Message::User { content } => content,
        _ => panic!("expected user message"),
    };
    assert!(content.iter().any(
        |block| matches!(block, ContentBlock::Image { mime_type, .. } if mime_type == "image/png")
    ));
}

#[test]
fn stdin_prompt_is_appended_with_separator() {
    assert_eq!(
        merge_stdin_prompt("summarize", Some("stdin text")),
        "summarize\n\nstdin text"
    );
    assert_eq!(merge_stdin_prompt("", Some("stdin text")), "stdin text");
    assert_eq!(merge_stdin_prompt("prompt", None), "prompt");
}

#[test]
fn parses_models_rotation_globs_and_thinking_levels() {
    let rotation = parse_model_rotation("claude-*:high,deepseek-chat:low").unwrap();
    assert_eq!(rotation.entries.len(), 2);
    assert_eq!(rotation.entries[0].pattern, "claude-*");
    assert_eq!(rotation.entries[0].thinking, Some(ThinkingLevel::High));
    assert_eq!(rotation.entries[1].pattern, "deepseek-chat");
    assert!(rotation.matches("claude-sonnet-4-5"));
    assert!(!rotation.matches("gpt-4"));
}

#[test]
fn models_rotation_and_provider_select_matching_model() {
    let invocation = CodingAgentInvocationOptions {
        prompt: Some("hello".into()),
        provider: Some("anthropic".into()),
        model_rotation: Some("*sonnet*".into()),
        ..Default::default()
    };

    let model = select_model(&invocation, None, None, None).unwrap();

    assert_eq!(model.provider, "anthropic");
    assert!(
        model.id.contains("sonnet") || model.name.to_ascii_lowercase().contains("sonnet"),
        "{} / {}",
        model.id,
        model.name
    );
}

#[test]
fn tool_filter_applies_allow_deny_and_disable_policies() {
    let tools = builtin_tools(tempfile::tempdir().unwrap().path().to_path_buf()).unwrap();
    let filtered = filter_tools(
        tools,
        &ToolFilter {
            allow: vec!["read".into(), "bash".into()],
            deny: vec!["bash".into()],
            no_tools: false,
            no_builtin_tools: false,
        },
    );
    let names: Vec<_> = filtered.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["read"]);

    let none = filter_tools(
        filtered,
        &ToolFilter {
            no_tools: true,
            ..Default::default()
        },
    );
    assert!(none.is_empty());
}

#[test]
fn no_builtin_tools_keeps_custom_tools() {
    let mut tools = builtin_tools(tempfile::tempdir().unwrap().path().to_path_buf()).unwrap();
    tools.push(agent_core::api::tool::AgentTool::new_text(
        "custom",
        "custom tool",
        serde_json::json!({"type":"object"}),
        |_, _| async { Ok("custom".to_string()) },
    ));

    let filtered = filter_tools(
        tools,
        &ToolFilter {
            no_builtin_tools: true,
            ..Default::default()
        },
    );

    let names: Vec<_> = filtered.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["custom"]);
}

#[test]
fn product_input_budgets_reject_oversized_files_before_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("large.txt");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(crate::limits::MAX_AT_FILE_BYTES as u64 + 1)
        .unwrap();

    let error = process_at_file_references("@large.txt", temp.path()).expect_err("oversized @file");

    assert!(error.to_string().contains("safety limit"));
}

#[test]
fn context_and_theme_loaders_apply_file_and_diagnostic_budgets() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let themes = temp.path().join("themes");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&themes).unwrap();

    let context = std::fs::File::create(cwd.join("AGENTS.md")).unwrap();
    context
        .set_len(crate::limits::MAX_CONTEXT_FILE_BYTES as u64 + 1)
        .unwrap();
    assert!(discover_context_files(&cwd, temp.path(), false).is_empty());

    let theme = std::fs::File::create(themes.join("large.json")).unwrap();
    theme
        .set_len(crate::limits::MAX_THEME_FILE_BYTES as u64 + 1)
        .unwrap();
    let (loaded, diagnostics) = load_themes(std::slice::from_ref(&themes));
    assert!(loaded.is_empty());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "theme_read_error"
                && diagnostic.message.contains("safety limit"))
    );
}
