use std::path::Path;

use hooks_guru::{chunk_files, default_max_arg_length, parse_command, relative_to, task_command};

#[test]
fn command_strings_split_like_lint_staged() {
    assert_eq!(parse_command("cargo fmt --"), ["cargo", "fmt", "--"]);
    assert_eq!(parse_command("prettier --write"), ["prettier", "--write"]);
    assert_eq!(
        parse_command(r#"eslint --fix "src/a b.ts" 'c d.ts'"#),
        ["eslint", "--fix", "src/a b.ts", "c d.ts"]
    );
    assert_eq!(parse_command(r#"sh -c "a && b""#), ["sh", "-c", "a && b"]);
    assert_eq!(parse_command(r#"echo a\ b"#), ["echo", r"a\", "b"]);
    assert_eq!(parse_command(r#"tool """#), ["tool", ""]);
    assert_eq!(parse_command("   "), Vec::<String>::new());
    assert_eq!(parse_command("it's"), ["it", "s"]);
    assert_eq!(parse_command("don't stop"), ["don", "t", "stop"]);
    assert_eq!(parse_command("a'b'c"), ["a'b'c"]);
    assert_eq!(parse_command("'quoted'after"), ["quoted", "after"]);
    assert_eq!(parse_command(r#"say "a\"b""#), ["say", r"a\", "b"]);
    assert_eq!(parse_command("x 'unterminated"), ["x", "unterminated"]);
    assert_eq!(parse_command(r#"a"b c"d"#), [r#"a"b c"d"#]);
}

#[test]
fn files_are_appended_to_task_commands() {
    let command = task_command(
        "cargo fmt --",
        &["C:/repo/a.rs".to_string(), "C:/repo/b.rs".to_string()],
        Path::new("C:/repo"),
    );
    assert_eq!(command.program, "cargo");
    assert_eq!(command.args, ["fmt", "--", "C:/repo/a.rs", "C:/repo/b.rs"]);
    assert_eq!(
        command.to_string(),
        "cargo fmt -- C:/repo/a.rs C:/repo/b.rs"
    );
}

#[test]
fn chunking_follows_lint_staged() {
    assert_eq!(
        default_max_arg_length(),
        if cfg!(windows) {
            8_191
        } else if cfg!(target_os = "macos") {
            262_144
        } else {
            131_072
        }
    );
    let files: Vec<String> = (0..10).map(|i| i.to_string()).collect();
    assert_eq!(chunk_files(files.clone(), None), vec![files.clone()]);
    assert_eq!(chunk_files(files.clone(), Some(1_000)), vec![files.clone()]);
    assert_eq!(
        chunk_files(files.clone(), Some(18)),
        vec![files[..5].to_vec(), files[5..].to_vec()]
    );
    assert_eq!(chunk_files(Vec::new(), Some(10)), Vec::<Vec<String>>::new());
    let many: Vec<String> = (0..100).map(|_| "x".repeat(50)).collect();
    let chunks = chunk_files(many.clone(), Some(1_000));
    assert_eq!(chunks.len(), 6);
    assert_eq!(chunks.concat(), many);
}

#[test]
fn relative_paths_keep_directory_boundaries() {
    let relative = Path::new("C:/repo");
    assert_eq!(
        relative_to(Path::new("C:/repo/src/a.rs"), relative),
        "src/a.rs"
    );
    assert_eq!(relative_to(Path::new("C:/repo/a.rs"), relative), "a.rs");
    assert_eq!(
        relative_to(Path::new("C:/other/a.rs"), relative),
        "../other/a.rs"
    );
    assert_eq!(
        relative_to(Path::new("C:/repo/other/a.rs"), Path::new("C:/repo/src")),
        "../other/a.rs"
    );
    assert_eq!(
        relative_to(Path::new("/abs/a.rs"), Path::new("/abs")),
        "a.rs"
    );
}
