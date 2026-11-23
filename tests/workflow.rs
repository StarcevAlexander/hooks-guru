use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use hooks_guru::{
    CommandResult, Config, Executor, GitRepo, HookOptions, ProcessExecutor, TaskCommand, hooks,
    run_hook,
};

struct Lab {
    root: PathBuf,
    repo: GitRepo,
}

impl Lab {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("hooks-guru-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "lab@example.com"]);
        git(&root, &["config", "user.name", "Lab"]);
        git(&root, &["config", "core.autocrlf", "false"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        let repo = GitRepo::discover(&root).unwrap();
        Self { root, repo }
    }

    fn commit(&self, name: &str, content: &str) {
        self.write(name, content);
        git(&self.root, &["add", "--", name]);
        git(&self.root, &["commit", "-q", "-m", "init"]);
    }

    fn write(&self, name: &str, content: &str) {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn work(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join(name)).unwrap()
    }

    fn index(&self, name: &str) -> String {
        let show = format!(":{name}");
        git(&self.root, &["show", &show])
    }

    fn head(&self, name: &str) -> String {
        let show = format!("HEAD:{name}");
        git(&self.root, &["show", &show])
    }

    fn stashes(&self) -> String {
        self.repo.git(&["stash", "list"]).unwrap()
    }

    fn has_step(&self, report: &hooks_guru::RunReport, title: &str) -> bool {
        report.steps.iter().any(|step| step.title.contains(title))
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Fake<F>(F);

impl<F: Fn(&TaskCommand) -> CommandResult + Send + Sync> Executor for Fake<F> {
    fn execute(&self, command: &TaskCommand) -> CommandResult {
        (self.0)(command)
    }
}

fn success(output: &str) -> CommandResult {
    CommandResult {
        code: Some(0),
        spawn_error: None,
        output: output.to_string(),
    }
}

fn failure() -> CommandResult {
    CommandResult {
        code: Some(1),
        spawn_error: None,
        output: String::from("boom\n"),
    }
}

fn files_of(command: &TaskCommand) -> Vec<PathBuf> {
    command
        .args
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .collect()
}

fn upper() -> impl Fn(&TaskCommand) -> CommandResult {
    |command| {
        for path in files_of(command) {
            let content = std::fs::read_to_string(&path).unwrap().to_uppercase();
            std::fs::write(&path, content).unwrap();
        }
        success("")
    }
}

fn hook(config: &str) -> hooks_guru::Hook {
    Config::parse(config)
        .unwrap()
        .hooks
        .into_values()
        .next()
        .unwrap()
}

fn options() -> HookOptions {
    HookOptions {
        concurrent: false,
        ..HookOptions::default()
    }
}

#[test]
fn tasks_format_staged_files_and_stage_the_result() {
    let lab = Lab::new("format");
    lab.commit("a.txt", "one\n");
    lab.write("a.txt", "one\ntwo\n");
    git(&lab.root, &["add", "--", "a.txt"]);

    let report = run_hook(
        &lab.repo,
        "pre-commit",
        &hook("[pre-commit]\n\"*.txt\" = \"upper\"\n"),
        &lab.root,
        &[],
        &options(),
        &Fake(upper()),
    );

    assert!(report.ok(), "{report:?}");
    assert_eq!(lab.index("a.txt"), "ONE\nTWO\n");
    assert_eq!(lab.work("a.txt"), "ONE\nTWO\n");
    assert_eq!(lab.head("a.txt"), "one\n");
    assert_eq!(report.tasks.len(), 1);
    assert_eq!(report.tasks[0].files, 1);
    assert_eq!(lab.stashes(), "");
    assert!(!lab.repo.patch_path().exists());
    assert!(lab.has_step(&report, "Applying modifications"));
    assert!(lab.has_step(&report, "Cleaning up"));
}

#[test]
fn partially_staged_files_keep_their_unstaged_changes() {
    let lab = Lab::new("partial");
    lab.commit("a.txt", "one\n");
    lab.write("a.txt", "one\ntwo\n");
    git(&lab.root, &["add", "--", "a.txt"]);
    lab.write("a.txt", "one\ntwo\nthree\n");

    let report = run_hook(
        &lab.repo,
        "pre-commit",
        &hook("[pre-commit]\n\"*.txt\" = \"upper\"\n"),
        &lab.root,
        &[],
        &options(),
        &Fake(upper()),
    );

    assert!(report.ok(), "{report:?}");
    assert!(lab.has_step(&report, "Hiding unstaged changes"));
    assert_eq!(lab.index("a.txt"), "ONE\nTWO\n");
    assert_eq!(lab.work("a.txt"), "ONE\nTWO\nthree\n");
    assert_eq!(lab.stashes(), "");
    assert!(!lab.repo.patch_path().exists());
}

#[test]
fn a_failing_task_reverts_the_repository() {
    let lab = Lab::new("revert");
    lab.commit("a.txt", "one\n");
    lab.write("a.txt", "one\ntwo\n");
    git(&lab.root, &["add", "--", "a.txt"]);
    lab.write("a.txt", "one\ntwo\nthree\n");

    let report = run_hook(
        &lab.repo,
        "pre-commit",
        &hook("[pre-commit]\n\"*.txt\" = \"upper\"\n"),
        &lab.root,
        &[],
        &options(),
        &Fake(|_: &TaskCommand| failure()),
    );

    assert!(!report.ok());
    assert_eq!(report.error.as_deref(), Some("1 task(s) failed"));
    assert_eq!(lab.index("a.txt"), "one\ntwo\n");
    assert_eq!(lab.work("a.txt"), "one\ntwo\nthree\n");
    assert_eq!(lab.stashes(), "");
    assert!(!lab.repo.patch_path().exists());
    assert!(lab.has_step(&report, "Reverting to original state"));
    let rendered = report.render(false);
    assert!(rendered.contains("✖"), "{rendered}");
    assert!(rendered.contains("boom"), "{rendered}");
}

#[test]
fn without_stash_a_failed_task_leaves_the_files_alone() {
    let lab = Lab::new("nostash");
    lab.commit("a.txt", "one\n");
    lab.write("a.txt", "one\ntwo\n");
    git(&lab.root, &["add", "--", "a.txt"]);
    lab.write("a.txt", "one\ntwo\nthree\n");
    let options = HookOptions {
        stash: false,
        ..options()
    };

    let report = run_hook(
        &lab.repo,
        "pre-commit",
        &hook("[pre-commit]\n\"*.txt\" = \"upper\"\n"),
        &lab.root,
        &[],
        &options,
        &Fake(|_: &TaskCommand| failure()),
    );

    assert!(!report.ok());
    assert!(!lab.has_step(&report, "Reverting"));
    assert_eq!(lab.index("a.txt"), "one\ntwo\n");
    assert_eq!(lab.work("a.txt"), "one\ntwo\nthree\n");
}

#[test]
fn nothing_staged_or_nothing_matching_is_a_no_op() {
    let lab = Lab::new("noop");
    lab.commit("a.txt", "one\n");
    let config = hook("[pre-commit]\n\"*.rs\" = \"upper\"\n");

    let clean = run_hook(
        &lab.repo,
        "pre-commit",
        &config,
        &lab.root,
        &[],
        &options(),
        &Fake(upper()),
    );
    assert!(clean.ok());
    assert!(clean.tasks.is_empty());
    assert!(lab.has_step(&clean, "No staged files found."));

    lab.write("a.txt", "changed\n");
    git(&lab.root, &["add", "--", "a.txt"]);
    let unmatched = run_hook(
        &lab.repo,
        "pre-commit",
        &config,
        &lab.root,
        &[],
        &options(),
        &Fake(upper()),
    );
    assert!(unmatched.ok());
    assert!(unmatched.tasks.is_empty());
    assert!(lab.has_step(&unmatched, "No staged files match any configured task."));
    assert_eq!(lab.index("a.txt"), "changed\n");
}

#[test]
fn patterns_match_basenames_and_run_per_group() {
    let lab = Lab::new("groups");
    lab.commit("keep.txt", "keep\n");
    for name in ["src/a.rs", "docs/b.rs", "c.txt"] {
        lab.write(name, "x\n");
    }
    git(&lab.root, &["add", "--", "."]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let executor: Fake<_> = Fake(move |command: &TaskCommand| {
        recorder.lock().unwrap().push(files_of(command));
        success("")
    });
    let config = hook("[pre-commit]\n\"*.rs\" = \"count\"\n\"*.txt\" = \"count\"\n");

    let report = run_hook(
        &lab.repo,
        "pre-commit",
        &config,
        &lab.root,
        &[],
        &options(),
        &executor,
    );

    assert!(report.ok(), "{report:?}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let names: Vec<Vec<String>> = seen
        .iter()
        .map(|files| {
            let mut names: Vec<String> = files
                .iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        })
        .collect();
    assert_eq!(names, [vec!["a.rs", "b.rs"], vec!["c.txt"]]);
}

#[test]
fn hook_commands_receive_the_hook_arguments() {
    let lab = Lab::new("args");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let executor: Fake<_> = Fake(move |command: &TaskCommand| {
        recorder.lock().unwrap().push(command.args.clone());
        success("")
    });
    let config = hook("[commit-msg]\ncommands = [\"linter --edit {}\", \"plain\"]\n");

    let report = run_hook(
        &lab.repo,
        "commit-msg",
        &config,
        &lab.root,
        &[String::from(".git/COMMIT_EDITMSG")],
        &options(),
        &executor,
    );

    assert!(report.ok(), "{report:?}");
    assert_eq!(
        *seen.lock().unwrap(),
        [vec!["--edit", ".git/COMMIT_EDITMSG"], vec![]]
    );
}

#[test]
fn relative_paths_are_passed_through() {
    let lab = Lab::new("relative");
    lab.commit("a.txt", "one\n");
    lab.write("a.txt", "one\ntwo\n");
    git(&lab.root, &["add", "--", "a.txt"]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let executor: Fake<_> = Fake(move |command: &TaskCommand| {
        recorder.lock().unwrap().push(command.args.clone());
        success("")
    });
    let options = HookOptions {
        relative: true,
        ..options()
    };

    let report = run_hook(
        &lab.repo,
        "pre-commit",
        &hook("[pre-commit]\n\"*.txt\" = \"count\"\n"),
        &lab.root,
        &[],
        &options,
        &executor,
    );

    assert!(report.ok());
    assert_eq!(*seen.lock().unwrap(), [vec!["a.txt"]]);
}

#[test]
fn an_emptied_commit_is_prevented_and_reverted() {
    let lab = Lab::new("empty");
    lab.commit("a.txt", "one\n");
    lab.write("a.txt", "one\ntwo\n");
    git(&lab.root, &["add", "--", "a.txt"]);
    let executor: Fake<_> = Fake(|command: &TaskCommand| {
        for path in files_of(command) {
            let head = lab.head("a.txt");
            std::fs::write(path, head).unwrap();
        }
        success("")
    });

    let report = run_hook(
        &lab.repo,
        "pre-commit",
        &hook("[pre-commit]\n\"*.txt\" = \"reset\"\n"),
        &lab.root,
        &[],
        &options(),
        &executor,
    );

    assert!(!report.ok());
    assert_eq!(
        report.error.as_deref(),
        Some("Prevented an empty git commit!")
    );
    assert_eq!(lab.index("a.txt"), "one\ntwo\n");
    assert_eq!(lab.work("a.txt"), "one\ntwo\n");
    assert_eq!(lab.stashes(), "");
}

#[test]
fn real_processes_run_in_the_repository() {
    let lab = Lab::new("process");
    lab.commit("a.txt", "one\n");
    lab.write("a.txt", "one\ntwo\n");
    git(&lab.root, &["add", "--", "a.txt"]);
    let config = hook("[pre-commit]\n\"*.txt\" = \"git --version\"\n");

    let report = run_hook(
        &lab.repo,
        "pre-commit",
        &config,
        &lab.root,
        &[],
        &options(),
        &ProcessExecutor::default(),
    );

    assert!(report.ok(), "{report:?}");
    assert!(report.tasks[0].result.output.contains("git version"));
    assert_eq!(report.tasks[0].command.cwd, lab.root);
}

#[test]
fn install_and_uninstall_manage_only_their_own_shims() {
    let lab = Lab::new("install");
    let config = Config::parse(
        "[pre-commit]\n\"*.rs\" = \"rustfmt\"\n[commit-msg]\ncommands = [\"linter\"]\n",
    )
    .unwrap();
    let executable = Path::new("/usr/local/bin/hooks-guru");
    let config_path = Path::new("/repo/hooks-guru.toml");

    let installed = hooks::install(
        &lab.repo,
        &config,
        config_path,
        ".githooks",
        executable,
        false,
    )
    .unwrap();
    assert_eq!(installed.len(), 2);
    let shim = std::fs::read_to_string(lab.root.join(".githooks").join("pre-commit")).unwrap();
    assert_eq!(
        shim,
        "#!/bin/sh\nexec \"/usr/local/bin/hooks-guru\" run \"pre-commit\" --config \"/repo/hooks-guru.toml\" \"$@\"\n"
    );
    assert!(hooks::is_managed(&shim, "pre-commit", executable));
    assert!(!hooks::is_managed(&shim, "commit-msg", executable));
    assert_eq!(
        lab.repo
            .config_get(hooks::HOOKS_PATH_KEY)
            .unwrap()
            .as_deref(),
        Some(".githooks")
    );

    lab.write(".githooks/pre-push", "#!/bin/sh\necho mine\n");
    let refused = hooks::install(
        &lab.repo,
        &Config::parse("[pre-push]\ncommands = [\"x\"]\n").unwrap(),
        config_path,
        ".githooks",
        executable,
        false,
    );
    assert!(refused.unwrap_err().to_string().contains("--force"));
    hooks::install(
        &lab.repo,
        &Config::parse("[pre-push]\ncommands = [\"x\"]\n").unwrap(),
        config_path,
        ".githooks",
        executable,
        true,
    )
    .unwrap();
    assert!(hooks::is_managed(
        &std::fs::read_to_string(lab.root.join(".githooks").join("pre-push")).unwrap(),
        "pre-push",
        executable
    ));

    let removed = hooks::uninstall(&lab.repo, ".githooks", executable).unwrap();
    assert_eq!(removed.len(), 3);
    assert!(!lab.root.join(".githooks").join("pre-commit").exists());
    assert_eq!(lab.repo.config_get(hooks::HOOKS_PATH_KEY).unwrap(), None);
}

#[test]
fn install_quotes_paths_for_the_shell() {
    let contents = hooks::shim_contents(
        "pre-commit",
        Path::new("C:/a $b/c.toml"),
        Path::new("C:/x/y.exe"),
    );
    assert_eq!(
        contents,
        "#!/bin/sh\nexec \"C:/x/y.exe\" run \"pre-commit\" --config \"C:/a \\$b/c.toml\" \"$@\"\n"
    );
}

#[test]
fn discover_reports_missing_repositories() {
    let root = std::env::temp_dir().join(format!("hooks-guru-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let error = GitRepo::discover(&root).unwrap_err();
    assert!(error.to_string().starts_with("git rev-parse"), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}
