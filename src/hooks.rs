use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::config::Config;
use crate::git::GitRepo;
use crate::tasks::io_error;

pub const DEFAULT_HOOKS_DIR: &str = ".githooks";
pub const HOOKS_PATH_KEY: &str = "core.hooksPath";

pub fn shim_contents(hook: &str, config: &Path, executable: &Path) -> String {
    format!(
        "#!/bin/sh\nexec {} run {} --config {} \"$@\"\n",
        quote(&posix(executable)),
        quote(hook),
        quote(&posix(config)),
    )
}

pub fn is_managed(contents: &str, hook: &str, executable: &Path) -> bool {
    contents.starts_with(&format!(
        "#!/bin/sh\nexec {} run {} ",
        quote(&posix(executable)),
        quote(hook)
    ))
}

pub fn shim_path(repo: &GitRepo, dir: &str, hook: &str) -> PathBuf {
    repo.top_level.join(dir).join(hook)
}

pub fn install(
    repo: &GitRepo,
    config: &Config,
    config_path: &Path,
    dir: &str,
    executable: &Path,
    force: bool,
) -> io::Result<Vec<PathBuf>> {
    fs::create_dir_all(repo.top_level.join(dir))?;
    let mut installed = Vec::new();
    for hook in config.hooks.keys() {
        let path = shim_path(repo, dir, hook);
        let contents = shim_contents(hook, config_path, executable);
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let foreign = !existing.is_empty()
            && existing != contents
            && !is_managed(&existing, hook, executable);
        if foreign && !force {
            return Err(io::Error::other(format!(
                "{} already exists and was not created by hooks-guru; pass --force to overwrite it",
                path.display()
            )));
        }
        fs::write(&path, contents)?;
        make_executable(&path)?;
        installed.push(path);
    }
    repo.config_set(HOOKS_PATH_KEY, dir).map_err(io_error)?;
    Ok(installed)
}

pub fn uninstall(repo: &GitRepo, dir: &str, executable: &Path) -> io::Result<Vec<PathBuf>> {
    let root = repo.top_level.join(dir);
    let mut removed = Vec::new();
    if root.is_dir() {
        for entry in fs::read_dir(&root)? {
            let path = entry?.path();
            let Some(hook) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let managed =
                fs::read_to_string(&path).is_ok_and(|text| is_managed(&text, hook, executable));
            if managed {
                fs::remove_file(&path)?;
                removed.push(path);
            }
        }
        if fs::read_dir(&root)?.next().is_none() {
            let _ = fs::remove_dir(&root);
        }
    }
    if repo
        .config_get(HOOKS_PATH_KEY)
        .map_err(io_error)?
        .is_some_and(|value| value == dir)
    {
        repo.config_unset(HOOKS_PATH_KEY).map_err(io_error)?;
    }
    Ok(removed)
}

fn posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}
