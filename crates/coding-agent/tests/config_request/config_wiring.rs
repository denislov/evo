use crate::internal_tests::support;

use crate::internal_tests::product_fixture::configuration::{
    KeySource, PartialSettings, load_config, resolve_api_key,
};
use coding_agent::api::embedding::CodingAgentInvocationOptions;
use support::EnvGuard;

#[test]
fn select_model_uses_default_model_when_no_flag() {
    use crate::internal_tests::product_fixture::configuration::select_model;
    let invocation = CodingAgentInvocationOptions::default();
    // default_model resolves via lookup_model; use a known built-in id.
    let model = select_model(&invocation, None, Some("claude-sonnet-4-5"), None).expect("model");
    assert_eq!(model.id, "claude-sonnet-4-5");
}

#[test]
fn load_config_from_temp_evo_dir() {
    let env = EnvGuard::new(&["EVO_DIR"]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("settings.toml"),
        "default_model = \"claude-sonnet-4-5\"\n",
    )
    .unwrap();
    env.set_evo_dir(dir.path());
    let (cfg, diags) = load_config(std::path::Path::new("."));
    assert_eq!(
        cfg.settings.default_model.as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert!(diags.is_empty());
}

#[test]
fn config_auth_resolution_prefers_env_over_auth_file() {
    let env = EnvGuard::new(&["EVO_DIR", "ANTHROPIC_API_KEY"]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("auth.toml"),
        "[anthropic]\ntype = \"api_key\"\nkey = \"from-auth\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join("auth.toml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    env.set_evo_dir(dir.path());
    env.set("ANTHROPIC_API_KEY", "from-env");

    let (cfg, diags) = load_config(std::path::Path::new("."));
    let mut key_diags = Vec::new();
    let key = resolve_api_key("anthropic", None, &cfg.auth, &mut key_diags).expect("key");

    assert_eq!(key.value, "from-env");
    assert_eq!(key.source, KeySource::Env);
    assert!(diags.is_empty());
    assert!(key_diags.is_empty());
}

#[test]
fn runtime_setting_helpers_consume_session_dir_and_context_flag() {
    use crate::internal_tests::product_fixture::configuration::{
        effective_no_context_files, effective_session_dir,
    };

    let invocation = CodingAgentInvocationOptions::default();
    let mut settings = PartialSettings {
        session_dir: Some("/tmp/evo-sessions".into()),
        no_context_files: Some(true),
        ..Default::default()
    }
    .resolve();

    assert_eq!(
        effective_session_dir(&invocation, &settings).as_deref(),
        Some(std::path::Path::new("/tmp/evo-sessions"))
    );
    assert!(effective_no_context_files(&invocation, &settings));

    let invocation = CodingAgentInvocationOptions {
        session_dir: Some("/tmp/cli-sessions".into()),
        ..Default::default()
    };
    settings.no_context_files = false;
    assert_eq!(
        effective_session_dir(&invocation, &settings).as_deref(),
        Some(std::path::Path::new("/tmp/cli-sessions"))
    );
    assert!(!effective_no_context_files(&invocation, &settings));
}
