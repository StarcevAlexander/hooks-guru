use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{Hook, HookOptions, TaskGroup};
use crate::git::{GitError, GitRepo, StagedFile};
use crate::glob::Glob;
use crate::tasks::{self, CommandResult, Executor, TaskCommand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepStatus {
    Done,
    Skipped,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub title: String,
    pub status: StepStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskReport {
    pub pattern: Option<String>,
    pub command: TaskCommand,
    pub files: usize,
    pub result: CommandResult,
    pub duration: Duration,
}

impl TaskReport {
    pub fn success(&self) -> bool {
        self.result.success()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunReport {
    pub hook: String,
    pub steps: Vec<Step>,
    pub tasks: Vec<TaskReport>,
    pub error: Option<String>,
}

impl RunReport {
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    pub fn render(&self, verbose: bool) -> String {
        let mut lines: Vec<String> = self
            .steps
            .iter()
            .map(|step| format!("{} {}", sign(step.status), step.title))
            .collect();
        let mut previous: Option<&str> = None;
        for task in &self.tasks {
            let failed = !task.success();
            if !verbose && !failed {
                continue;
            }
            let status = if failed {
                StepStatus::Failed
            } else {
                StepStatus::Done
            };
            match task.pattern.as_deref() {
                Some(pattern) => {
                    if previous != Some(pattern) {
                        lines.push(format!("  {pattern}"));
                        previous = Some(pattern);
                    }
                    lines.push(format!(
                        "    {} {} — {}",
                        sign(status),
                        task.command,
                        describe(task)
                    ));
                }
                None => lines.push(format!(
                    "  {} {} — {}",
                    sign(status),
                    task.command,
                    describe(task)
                )),
            }
            if failed {
                lines.push(String::new());
                lines.extend(
                    task.result
                        .output
                        .lines()
                        .map(|line| format!("      {line}")),
                );
                lines.push(String::new());
            }
        }
        if let Some(error) = &self.error {
            lines.push(format!("{} {error}", sign(StepStatus::Failed)));
        }
        lines.join("\n")
    }
}

struct Planned {
    group: TaskGroup,
    files: Vec<String>,
    sources: Vec<PathBuf>,
}

pub fn run_hook(
    repo: &GitRepo,
    hook: &str,
    config: &Hook,
    config_dir: &Path,
    args: &[String],
    options: &HookOptions,
    executor: &dyn Executor,
) -> RunReport {
    let mut report = RunReport {
        hook: hook.to_string(),
        steps: Vec::new(),
        tasks: Vec::new(),
        error: None,
    };
    let mut backup: Option<String> = None;
    let mut patch: Option<PathBuf> = None;
    let mut planned: Vec<Planned> = Vec::new();

    report.step("Preparing hooks-guru...", StepStatus::Done);
    let groups = config.task_groups();
    if !groups.is_empty() {
        match repo.staged_files(options.diff.as_deref(), &options.diff_filter) {
            Err(error) => {
                return report.fail("Failed to get staged files!", error.to_string());
            }
            Ok(staged) if staged.is_empty() => {
                report.step("No staged files found.", StepStatus::Skipped);
            }
            Ok(staged) => {
                planned = plan(&staged, &groups, config_dir, options.relative);
                if planned.is_empty() {
                    report.step(
                        "No staged files match any configured task.",
                        StepStatus::Skipped,
                    );
                }
            }
        }
    }

    if !planned.is_empty() {
        if options.stash && repo.has_initial_commit() {
            match repo.create_stash().and_then(|stash| match stash {
                Some(hash) => repo.store_stash(&hash).map(|()| Some(hash)),
                None => Ok(None),
            }) {
                Ok(Some(hash)) => {
                    let short = hash.get(..7).unwrap_or(&hash).to_string();
                    backup = Some(hash);
                    report.step(
                        format!("Backed up original state in git stash ({short})"),
                        StepStatus::Done,
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    drop_backup(repo, backup.as_deref());
                    return report.fail("Failed to back up the original state!", error.to_string());
                }
            }
        }
        if options.hide_partially_staged {
            match repo.partially_staged_files() {
                Err(error) => {
                    drop_backup(repo, backup.as_deref());
                    return report
                        .fail("Failed to find partially staged files!", error.to_string());
                }
                Ok(files) if files.is_empty() => {
                    report.step("No partially staged files to hide.", StepStatus::Skipped);
                }
                Ok(files) => {
                    let path = repo.patch_path();
                    let hidden = repo
                        .save_unstaged_patch(&files, &path)
                        .and_then(|()| repo.checkout_force(&files));
                    match hidden {
                        Ok(()) => {
                            patch = Some(path);
                            report.step(
                                "Hiding unstaged changes to partially staged files...",
                                StepStatus::Done,
                            );
                        }
                        Err(error) => {
                            let _ = std::fs::remove_file(&path);
                            drop_backup(repo, backup.as_deref());
                            return report.fail(
                                "Failed to hide unstaged changes to partially staged files!",
                                error.to_string(),
                            );
                        }
                    }
                }
            }
        }
    }

    if !planned.is_empty() {
        report.step("Running tasks for staged files...", StepStatus::Done);
        report.tasks = execute_tasks(&planned, config_dir, &repo.top_level, options, executor);
    }
    if report.tasks.iter().all(TaskReport::success) && !config.commands.is_empty() {
        report.step("Running hook commands...", StepStatus::Done);
        for command in &config.commands {
            let mut task = tasks::substitute_hook_args(command, args);
            task.cwd = repo.top_level.clone();
            let started = Instant::now();
            let result = executor.execute(&task);
            let success = result.success();
            report.tasks.push(TaskReport {
                pattern: None,
                command: task,
                files: args.len(),
                result,
                duration: started.elapsed(),
            });
            if !success {
                break;
            }
        }
    }
    let failed = report.tasks.iter().filter(|task| !task.success()).count();
    if failed > 0 {
        let error = format!("{failed} task(s) failed");
        return finish(
            repo,
            report,
            backup.as_deref(),
            patch.as_deref(),
            Some(error),
        );
    }

    if !planned.is_empty() {
        report.step("Applying modifications from tasks...", StepStatus::Done);
        let mut sources: Vec<String> = planned
            .iter()
            .flat_map(|task| task.sources.iter())
            .filter(|path| path.exists())
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        sources.sort();
        sources.dedup();
        if let Err(error) = repo.add(&sources) {
            let message = format!("Failed to add task modifications: {error}");
            return finish(
                repo,
                report,
                backup.as_deref(),
                patch.as_deref(),
                Some(message),
            );
        }
        let staged = repo.staged_files(options.diff.as_deref(), &options.diff_filter);
        let empty = match &staged {
            Ok(files) => files.is_empty(),
            Err(error) => {
                let message = format!("Failed to read staged files: {error}");
                return finish(
                    repo,
                    report,
                    backup.as_deref(),
                    patch.as_deref(),
                    Some(message),
                );
            }
        };
        if empty && !options.allow_empty {
            let message = String::from("Prevented an empty git commit!");
            return finish(
                repo,
                report,
                backup.as_deref(),
                patch.as_deref(),
                Some(message),
            );
        }
    }

    if let Some(path) = &patch {
        report.step("Restoring unstaged changes...", StepStatus::Done);
        if let Err(error) = repo
            .apply_patch(path, false)
            .or_else(|_| repo.apply_patch(path, true))
        {
            report.fail_last_step();
            let kept = match &backup {
                Some(hash) => {
                    format!(
                        "The backup stash {hash} was kept, restore it with `git stash apply --index`."
                    )
                }
                None => format!("The patch was kept at {}.", path.display()),
            };
            report.error = Some(format!(
                "Unstaged changes could not be restored due to a merge conflict! {kept} ({error})"
            ));
            return report;
        }
    }

    report.step("Cleaning up temporary files...", StepStatus::Done);
    drop_backup(repo, backup.as_deref());
    if let Some(path) = &patch {
        let _ = std::fs::remove_file(path);
    }
    report
}

fn finish(
    repo: &GitRepo,
    mut report: RunReport,
    backup: Option<&str>,
    patch: Option<&Path>,
    error: Option<String>,
) -> RunReport {
    report.error = error;
    if let Some(hash) = backup {
        report.step(
            "Reverting to original state because of errors...",
            StepStatus::Done,
        );
        if let Err(restore) = restore_original(repo, hash) {
            report.fail_last_step();
            let error = report.error.take().unwrap_or_default();
            report.error = Some(format!(
                "{error} The original state could not be restored ({restore}); recover it with `git stash apply --index` from stash {hash}."
            ));
            return report;
        }
    } else if let Some(path) = patch {
        let _ = repo
            .apply_patch(path, false)
            .or_else(|_| repo.apply_patch(path, true));
    }
    report.step("Cleaning up temporary files...", StepStatus::Done);
    if let Some(path) = patch {
        let _ = std::fs::remove_file(path);
    }
    report
}

fn restore_original(repo: &GitRepo, hash: &str) -> Result<(), String> {
    let restore = || -> Result<(), GitError> {
        repo.reset_hard()?;
        let index = repo
            .stash_index(hash)?
            .ok_or_else(|| GitError::new("stash list", "the backup stash is missing"))?;
        repo.apply_stash(index)?;
        repo.drop_stash(index)
    };
    restore().map_err(|error| error.to_string())
}

fn drop_backup(repo: &GitRepo, backup: Option<&str>) {
    if let Some(hash) = backup
        && let Ok(Some(index)) = repo.stash_index(hash)
    {
        let _ = repo.drop_stash(index);
    }
}

fn plan(
    staged: &[StagedFile],
    groups: &[TaskGroup],
    config_dir: &Path,
    relative: bool,
) -> Vec<Planned> {
    let positions: Vec<(String, PathBuf)> = staged
        .iter()
        .map(|file| (relative_to(&file.path, config_dir), file.path.clone()))
        .collect();
    groups
        .iter()
        .filter_map(|group| {
            let glob = Glob::new(&group.pattern).ok()?;
            let parent = group.pattern.starts_with("../");
            let files: Vec<(String, PathBuf)> = positions
                .iter()
                .filter(|(relative_path, _)| {
                    let inside = !relative_path.starts_with("../")
                        && !Path::new(relative_path).is_absolute();
                    (parent || inside) && glob.is_match(relative_path)
                })
                .cloned()
                .collect();
            if files.is_empty() {
                return None;
            }
            Some(Planned {
                sources: files.iter().map(|(_, path)| path.clone()).collect(),
                files: files
                    .iter()
                    .map(|(relative_path, path)| {
                        if relative {
                            relative_path.clone()
                        } else {
                            path.to_string_lossy().replace('\\', "/")
                        }
                    })
                    .collect(),
                group: group.clone(),
            })
        })
        .collect()
}

pub fn relative_to(target: &Path, base: &Path) -> String {
    let target_parts: Vec<Component> = target.components().collect();
    let base_parts: Vec<Component> = base.components().collect();
    let common = target_parts
        .iter()
        .zip(&base_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let root = |part: Option<&Component>| {
        matches!(
            part,
            Some(Component::RootDir | Component::Prefix(_) | Component::ParentDir)
        )
    };
    if common == 0 || root(base_parts.get(common)) || root(target_parts.get(common)) {
        return target.to_string_lossy().replace('\\', "/");
    }
    let mut parts: Vec<String> = (common..base_parts.len())
        .map(|_| String::from(".."))
        .collect();
    parts.extend(
        target_parts[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

fn execute_tasks(
    planned: &[Planned],
    config_dir: &Path,
    top_level: &Path,
    options: &HookOptions,
    executor: &dyn Executor,
) -> Vec<TaskReport> {
    let max_arg_length = options
        .max_arg_length
        .unwrap_or_else(tasks::default_max_arg_length);
    let run_task = |task: &Planned| -> Vec<TaskReport> {
        let mut reports = Vec::new();
        for chunk in tasks::chunk_files(task.files.clone(), Some(max_arg_length)) {
            for command in &task.group.commands {
                let mut command = tasks::task_command(command, &chunk, config_dir);
                if Path::new(&command.program)
                    .file_stem()
                    .is_some_and(|stem| stem.eq_ignore_ascii_case("git"))
                {
                    command.cwd = top_level.to_path_buf();
                }
                let files = chunk.len();
                let started = Instant::now();
                let result = executor.execute(&command);
                let success = result.success();
                reports.push(TaskReport {
                    pattern: Some(task.group.pattern.clone()),
                    command,
                    files,
                    result,
                    duration: started.elapsed(),
                });
                if !success {
                    return reports;
                }
            }
        }
        reports
    };
    if !options.concurrent || planned.len() < 2 {
        return planned.iter().flat_map(run_task).collect();
    }
    let results = Mutex::new(Vec::new());
    let shared = &results;
    let run_task = &run_task;
    std::thread::scope(|scope| {
        for (index, task) in planned.iter().enumerate() {
            scope.spawn(move || {
                let reports = run_task(task);
                shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push((index, reports));
            });
        }
    });
    let mut results = results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    results.sort_by_key(|(index, _)| *index);
    results
        .into_iter()
        .flat_map(|(_, reports)| reports)
        .collect()
}

impl RunReport {
    fn step(&mut self, title: impl Into<String>, status: StepStatus) {
        self.steps.push(Step {
            title: title.into(),
            status,
        });
    }

    fn fail(mut self, title: &str, error: String) -> RunReport {
        self.step(title, StepStatus::Failed);
        self.error = Some(error);
        self
    }

    fn fail_last_step(&mut self) {
        if let Some(step) = self.steps.last_mut() {
            step.status = StepStatus::Failed;
        }
    }
}

fn describe(task: &TaskReport) -> String {
    match task.result.failure() {
        Some(failure) => failure,
        None => format!("Completed in {}", tasks::format_duration(task.duration)),
    }
}

fn sign(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Done => "✔",
        StepStatus::Skipped => "—",
        StepStatus::Failed => "✖",
    }
}
