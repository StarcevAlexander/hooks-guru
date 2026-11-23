use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Table;

use crate::glob::Glob;

pub const CONFIG_FILES: [&str; 3] = ["hooks-guru.toml", ".hooks-guru.toml", ".githooks.toml"];
pub const TOOL_SECTION: &str = "hooks-guru";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(flatten)]
    pub hooks: BTreeMap<String, Hook>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Hook {
    pub commands: Vec<String>,
    #[serde(flatten)]
    pub tasks: Table,
    pub options: HookOptions,
}

impl Default for Hook {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            tasks: Table::new(),
            options: HookOptions::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HookOptions {
    pub concurrent: bool,
    pub relative: bool,
    pub stash: bool,
    pub allow_empty: bool,
    pub hide_partially_staged: bool,
    pub max_arg_length: Option<usize>,
    pub diff: Option<String>,
    pub diff_filter: String,
}

impl Default for HookOptions {
    fn default() -> Self {
        Self {
            concurrent: true,
            relative: false,
            stash: true,
            allow_empty: false,
            hide_partially_staged: true,
            max_arg_length: None,
            diff: None,
            diff_filter: String::from("ACMR"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskGroup {
    pub pattern: String,
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(input)
            .map_err(|error| ConfigError::new(format!("invalid TOML: {error}")))?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_manifest(manifest: &str) -> Result<Self, ConfigError> {
        manifest_section(manifest)?
            .ok_or_else(|| ConfigError::new(format!("no [tool.{TOOL_SECTION}] section")))
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let input = std::fs::read_to_string(path)
            .map_err(|error| ConfigError::new(format!("{}: {error}", path.display())))?;
        Self::parse(&input)
            .map_err(|error| ConfigError::new(format!("{}: {error}", path.display())))
    }

    pub fn discover(cwd: &Path) -> Result<Option<(PathBuf, Self)>, ConfigError> {
        let mut current = Some(cwd);
        while let Some(dir) = current {
            for name in CONFIG_FILES {
                let path = dir.join(name);
                if path.is_file() {
                    return Self::load(&path).map(|config| Some((path, config)));
                }
            }
            let manifest = dir.join("Cargo.toml");
            if let Ok(text) = std::fs::read_to_string(&manifest)
                && let Some(config) = manifest_section(&text)?
            {
                return Ok(Some((manifest, config)));
            }
            current = dir.parent();
        }
        Ok(None)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.hooks.is_empty() {
            return Err(ConfigError::new("no hooks are configured"));
        }
        for (name, hook) in &self.hooks {
            let valid = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !valid {
                return Err(ConfigError::new(format!("invalid hook name {name:?}")));
            }
            if hook.commands.is_empty() && hook.tasks.is_empty() {
                return Err(ConfigError::new(format!(
                    "hook {name:?} has no commands and no tasks"
                )));
            }
            for command in &hook.commands {
                if command.trim().is_empty() {
                    return Err(ConfigError::new(format!(
                        "hook {name:?} has an empty command"
                    )));
                }
            }
            for (pattern, value) in &hook.tasks {
                if pattern.trim().is_empty() {
                    return Err(ConfigError::new(format!(
                        "hook {name:?} has an empty task pattern"
                    )));
                }
                Glob::new(pattern)
                    .map_err(|error| ConfigError::new(format!("hook {name:?}: {error}")))?;
                if task_commands(value)?.is_empty() {
                    return Err(ConfigError::new(format!(
                        "hook {name:?} task {pattern:?} has no commands"
                    )));
                }
            }
            let filter = &hook.options.diff_filter;
            if filter.is_empty() || !filter.chars().all(|c| "ACDMRTUXB".contains(c)) {
                return Err(ConfigError::new(format!(
                    "hook {name:?} has an invalid diff_filter {filter:?}"
                )));
            }
            if hook
                .options
                .diff
                .as_ref()
                .is_some_and(|diff| diff.trim().is_empty())
            {
                return Err(ConfigError::new(format!("hook {name:?} has an empty diff")));
            }
            if hook.options.max_arg_length == Some(0) {
                return Err(ConfigError::new(format!(
                    "hook {name:?} has a zero max_arg_length"
                )));
            }
        }
        Ok(())
    }
}

impl Hook {
    pub fn task_groups(&self) -> Vec<TaskGroup> {
        self.tasks
            .iter()
            .filter_map(|(pattern, value)| {
                task_commands(value).ok().map(|commands| TaskGroup {
                    pattern: pattern.clone(),
                    commands,
                })
            })
            .collect()
    }
}

pub fn split_commands(command: &str) -> Vec<String> {
    command
        .split("&&")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn task_commands(value: &toml::Value) -> Result<Vec<String>, ConfigError> {
    let items = match value {
        toml::Value::String(command) => return Ok(split_commands(command)),
        toml::Value::Array(items) => items,
        _ => {
            return Err(ConfigError::new(
                "a task must be a command or an array of commands",
            ));
        }
    };
    let mut commands = Vec::new();
    for item in items {
        let command = item
            .as_str()
            .ok_or_else(|| ConfigError::new("a task must be a command or an array of commands"))?;
        commands.extend(split_commands(command));
    }
    Ok(commands)
}

fn manifest_section(manifest: &str) -> Result<Option<Config>, ConfigError> {
    let value: toml::Value = toml::from_str(manifest)
        .map_err(|error| ConfigError::new(format!("invalid TOML: {error}")))?;
    let Some(section) = value.get("tool").and_then(|tool| tool.get(TOOL_SECTION)) else {
        return Ok(None);
    };
    let config: Config = section
        .clone()
        .try_into()
        .map_err(|error| ConfigError::new(format!("invalid TOML: {error}")))?;
    config.validate()?;
    Ok(Some(config))
}
