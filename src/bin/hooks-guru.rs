use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hooks_guru::{CONFIG_FILES, Config, DEFAULT_HOOKS_DIR, GitRepo, HookOptions, ProcessExecutor};

const USAGE: &str = "hooks-guru - git hooks that run commands on staged files

Usage:
  hooks-guru run <hook> [options] [-- <hook arguments>]   run one hook
  hooks-guru install [options]                           create hook shims and point git at them
  hooks-guru uninstall [options]                         remove the shims created by install
  hooks-guru list [options]                              print the configured hooks
  hooks-guru --help | --version

Options:
  --config <file>           TOML config; found by walking up from the current directory
  --dir <path>              hooks directory for install and uninstall (default .githooks)
  --force                   overwrite hooks not created by hooks-guru
  --verbose                 list every task, including successful ones
  --color                   ask tasks for colored output

  run options:
  --no-stash                do not back the repository up in a stash
  --no-concurrent           run task groups one after another
  --relative                pass file paths relative to the config directory
  --allow-empty             let the tasks empty the commit
  --no-hide-partial         do not hide unstaged changes of partially staged files
  --diff <args>             diff arguments selecting the files, for example \"HEAD~1\"
  --diff-filter <letters>   git status letters to keep (default ACMR)
  --max-arg-length <n>      split many files into several command invocations

The config is a TOML file named hooks-guru.toml, .hooks-guru.toml or .githooks.toml, or the
[tool.hooks-guru] section of Cargo.toml:

  [pre-commit]
  \"*.rs\" = [\"cargo fmt --\", \"cargo clippy --\"]
  \"*.{json,yml,toml}\" = \"prettier --write\"

  [commit-msg]
  commands = [\"commitlint-guru --edit {}\"]

Exit codes: 0 when the hook passed or nothing ran, 1 when a task failed, 2 for usage errors.";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let code = match arguments.first().map(String::as_str) {
        Some("run") => run(&arguments[1..]),
        Some("install") => install(&arguments[1..]),
        Some("uninstall") => uninstall(&arguments[1..]),
        Some("list") => list(&arguments[1..]),
        Some("--version" | "-V") => {
            println!("hooks-guru {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Some("--help" | "-h") | None => {
            println!("{USAGE}");
            Ok(0)
        }
        Some(other) => Err(format!("unknown command {other:?}")),
    };
    match code {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("hooks-guru: {message}");
            ExitCode::from(2)
        }
    }
}

struct Flags {
    values: Vec<(String, Option<String>)>,
    rest: Vec<String>,
}

impl Flags {
    fn parse(arguments: &[String], with_values: &[&str]) -> Result<Self, String> {
        let mut flags = Self {
            values: Vec::new(),
            rest: Vec::new(),
        };
        let mut index = 0;
        let mut only_positional = false;
        while let Some(argument) = arguments.get(index) {
            index += 1;
            if only_positional || !argument.starts_with("--") || argument == "--" {
                if argument == "--" && !only_positional {
                    only_positional = true;
                } else {
                    flags.rest.push(argument.clone());
                }
                continue;
            }
            let value = if with_values.contains(&argument.as_str()) {
                Some(
                    arguments
                        .get(index)
                        .cloned()
                        .ok_or_else(|| format!("{argument} expects a value"))?,
                )
            } else {
                None
            };
            if value.is_some() {
                index += 1;
            }
            flags.values.push((argument.clone(), value));
        }
        Ok(flags)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(flag, _)| flag == name)
            .and_then(|(_, value)| value.as_deref())
    }

    fn present(&self, name: &str) -> bool {
        self.values.iter().any(|(flag, _)| flag == name)
    }
}

fn allowed(flags: &Flags, allowed: &[&str]) -> Result<(), String> {
    match flags
        .values
        .iter()
        .find(|(flag, _)| !allowed.contains(&flag.as_str()))
    {
        Some((flag, _)) => Err(format!("unknown option {flag:?}")),
        None => Ok(()),
    }
}

fn load_config(flags: &Flags) -> Result<Option<(PathBuf, Config)>, String> {
    match flags.value("--config") {
        Some(path) => {
            let path = PathBuf::from(path);
            let config = Config::load(&path).map_err(|error| error.to_string())?;
            Ok(Some((path, config)))
        }
        None => Config::discover(&std::env::current_dir().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string()),
    }
}

fn run(arguments: &[String]) -> Result<u8, String> {
    let flags = Flags::parse(
        arguments,
        &["--config", "--diff", "--diff-filter", "--max-arg-length"],
    )?;
    allowed(
        &flags,
        &[
            "--config",
            "--verbose",
            "--color",
            "--no-stash",
            "--no-concurrent",
            "--relative",
            "--allow-empty",
            "--no-hide-partial",
            "--diff",
            "--diff-filter",
            "--max-arg-length",
        ],
    )?;
    let (Some(hook), rest) = (flags.rest.first(), &flags.rest[1..]) else {
        return Err(String::from(
            "run needs a hook name, for example: hooks-guru run pre-commit",
        ));
    };
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let Some((config_path, config)) = load_config(&flags)? else {
        return Ok(0);
    };
    let Some(hook_config) = config.hooks.get(hook) else {
        println!("No configuration for hook {hook}.");
        return Ok(0);
    };
    let repo = GitRepo::discover(&cwd).map_err(|error| error.to_string())?;
    let mut options = HookOptions {
        concurrent: !flags.present("--no-concurrent"),
        relative: flags.present("--relative"),
        stash: !flags.present("--no-stash"),
        allow_empty: flags.present("--allow-empty"),
        hide_partially_staged: !flags.present("--no-hide-partial"),
        ..hook_config.options.clone()
    };
    if let Some(diff) = flags.value("--diff") {
        options.diff = Some(diff.to_string());
    }
    if let Some(filter) = flags.value("--diff-filter") {
        options.diff_filter = filter.to_string();
    }
    if let Some(length) = flags.value("--max-arg-length") {
        options.max_arg_length = Some(length.parse().map_err(|_| "invalid --max-arg-length")?);
    }
    let executor = ProcessExecutor {
        color: flags.present("--color"),
    };
    let report = hooks_guru::run_hook(
        &repo,
        hook,
        hook_config,
        config_path.parent().unwrap_or(Path::new(".")),
        rest,
        &options,
        &executor,
    );
    println!("{}", report.render(flags.present("--verbose")));
    Ok(u8::from(!report.ok()))
}

fn install(arguments: &[String]) -> Result<u8, String> {
    let flags = Flags::parse(arguments, &["--config", "--dir"])?;
    allowed(&flags, &["--config", "--dir", "--force"])?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let Some((config_path, config)) = load_config(&flags)? else {
        return Err(no_config());
    };
    let repo = GitRepo::discover(&cwd).map_err(|error| error.to_string())?;
    let dir = flags.value("--dir").unwrap_or(DEFAULT_HOOKS_DIR);
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let config_path = absolute(&config_path)?;
    let installed = hooks_guru::hooks::install(
        &repo,
        &config,
        &config_path,
        dir,
        &executable,
        flags.present("--force"),
    )
    .map_err(|error| error.to_string())?;
    for path in &installed {
        println!("Installed {}", path.display());
    }
    println!("Git runs hooks from {dir}.");
    Ok(0)
}

fn uninstall(arguments: &[String]) -> Result<u8, String> {
    let flags = Flags::parse(arguments, &["--dir"])?;
    allowed(&flags, &["--dir"])?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let repo = GitRepo::discover(&cwd).map_err(|error| error.to_string())?;
    let dir = flags.value("--dir").unwrap_or(DEFAULT_HOOKS_DIR);
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let removed =
        hooks_guru::hooks::uninstall(&repo, dir, &executable).map_err(|error| error.to_string())?;
    if removed.is_empty() {
        println!("No hooks-guru hooks found in {dir}.");
    }
    for path in &removed {
        println!("Removed {}", path.display());
    }
    Ok(0)
}

fn list(arguments: &[String]) -> Result<u8, String> {
    let flags = Flags::parse(arguments, &["--config"])?;
    allowed(&flags, &["--config"])?;
    match load_config(&flags)? {
        None => Err(no_config()),
        Some((path, config)) => {
            println!("{}", path.display());
            for (name, hook) in &config.hooks {
                println!("  {name}");
                for command in &hook.commands {
                    println!("    $ {command}");
                }
                for group in hook.task_groups() {
                    println!("    {}  {}", group.pattern, group.commands.join(" && "));
                }
            }
            Ok(0)
        }
    }
}

fn no_config() -> String {
    format!(
        "no configuration found; create one of {} or a [tool.hooks-guru] section in Cargo.toml",
        CONFIG_FILES.join(", ")
    )
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|error| error.to_string())
}
