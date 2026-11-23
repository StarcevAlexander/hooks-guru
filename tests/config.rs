use hooks_guru::{Config, HookOptions};

const FULL: &str = r#"
[pre-commit]
"*.rs" = ["cargo fmt --", "cargo clippy --"]
"*.{json,yml,toml}" = "prettier --write"
"*.md" = "prettier --write && markdownlint --fix"

[pre-commit.options]
concurrent = false

[commit-msg]
commands = ["commitlint-guru --edit {}"]
"#;

#[test]
fn parses_lint_staged_style_tasks() {
    let config = Config::parse(FULL).unwrap();
    assert_eq!(config.hooks.len(), 2);
    let pre_commit = &config.hooks["pre-commit"];
    let groups = pre_commit.task_groups();
    assert_eq!(
        groups
            .iter()
            .map(|g| g.pattern.as_str())
            .collect::<Vec<_>>(),
        ["*.rs", "*.{json,yml,toml}", "*.md"]
    );
    assert_eq!(groups[0].commands, ["cargo fmt --", "cargo clippy --"]);
    assert_eq!(groups[1].commands, ["prettier --write"]);
    assert_eq!(
        groups[2].commands,
        ["prettier --write", "markdownlint --fix"]
    );
    assert!(!pre_commit.options.concurrent);
    assert_eq!(
        config.hooks["commit-msg"].commands,
        ["commitlint-guru --edit {}"]
    );
    assert!(config.hooks["commit-msg"].tasks.is_empty());
}

#[test]
fn defaults_match_lint_staged() {
    let config = Config::parse("[pre-push]\ncommands = [\"cargo test\"]\n").unwrap();
    let options = &config.hooks["pre-push"].options;
    assert_eq!(
        options,
        &HookOptions {
            concurrent: true,
            relative: false,
            stash: true,
            allow_empty: false,
            hide_partially_staged: true,
            max_arg_length: None,
            diff: None,
            diff_filter: String::from("ACMR"),
        }
    );
}

#[test]
fn tool_section_of_a_cargo_manifest() {
    let manifest = r#"
[package]
name = "demo"
version = "0.1.0"

[tool.hooks-guru.pre-commit]
"*.rs" = "rustfmt"

[tool.hooks-guru.pre-commit.options]
diff = "HEAD~1"
"#;
    let config = Config::from_manifest(manifest).unwrap();
    assert_eq!(config.hooks.len(), 1);
    assert_eq!(
        config.hooks["pre-commit"].options.diff.as_deref(),
        Some("HEAD~1")
    );
    let plain = "[package]\nname = \"demo\"\n";
    assert!(Config::from_manifest(plain).is_err());
}

#[test]
fn invalid_configuration_is_rejected() {
    let errors = [
        "",
        "[pre-commit]\n",
        "[pre-commit]\n\"*.rs\" = []\n",
        "[pre-commit]\n\"*.rs\" = \"  \"\n",
        "[pre-commit]\n\"*.rs\" = 42\n",
        "[pre-commit]\n\"[a\" = \"x\"\n",
        "[pre-commit]\n\"\" = \"x\"\n",
        "[\"bad hook\"]\ncommands = [\"x\"]\n",
        "[pre-commit]\ncommands = [\"\"]\n",
        "[pre-commit]\ncommands = [\"x\"]\n[pre-commit.options]\ndiff_filter = \"\"\n",
        "[pre-commit]\ncommands = [\"x\"]\n[pre-commit.options]\nmax_arg_length = 0\n",
        "[pre-commit]\ncommands = [\"x\"]\n[pre-commit.options]\nnonsense = true\n",
        "[pre-commit]\ncommands = [\"x\"]\nno_such_key = 1\n",
        "[pre-commit]\ncommands = \"x\"\n",
    ];
    for input in errors {
        let error = Config::parse(input)
            .err()
            .unwrap_or_else(|| panic!("{input:?} must fail"));
        assert!(!error.message.is_empty());
    }
}

#[test]
fn discover_walks_up_to_the_config() {
    let root = std::env::temp_dir().join(format!("hooks-guru-discover-{}", std::process::id()));
    let nested = root.join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(Config::discover(&nested).unwrap().is_none());
    std::fs::write(
        root.join("hooks-guru.toml"),
        "[pre-commit]\ncommands = [\"x\"]\n",
    )
    .unwrap();
    let (found, _) = Config::discover(&nested).unwrap().unwrap();
    assert_eq!(
        std::fs::canonicalize(&found).unwrap(),
        std::fs::canonicalize(root.join("hooks-guru.toml")).unwrap()
    );
    std::fs::write(
        nested.join("Cargo.toml"),
        "[tool.hooks-guru.commit-msg]\ncommands = [\"y\"]\n",
    )
    .unwrap();
    let (found, config) = Config::discover(&nested).unwrap().unwrap();
    assert!(found.ends_with("Cargo.toml"));
    assert_eq!(config.hooks["commit-msg"].commands, ["y"]);
    std::fs::remove_dir_all(root).unwrap();
}
