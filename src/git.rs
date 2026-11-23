use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const STASH_MESSAGE: &str = "hooks-guru automatic backup";

const PATCH_FILE: &str = "hooks-guru_unstaged.patch";
const STAGED_STATUSES: &[u8] = b"AMDCRU";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedFile {
    pub path: PathBuf,
    pub status: char,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitError {
    pub command: String,
    pub message: String,
}

impl GitError {
    pub fn new(command: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "git {}: {}", self.command, self.message)
    }
}

impl std::error::Error for GitError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepo {
    pub top_level: PathBuf,
    pub git_dir: PathBuf,
}

impl GitRepo {
    pub fn discover(cwd: &Path) -> Result<Self, GitError> {
        let args = [
            "rev-parse",
            "--show-cdup",
            "--show-toplevel",
            "--absolute-git-dir",
        ];
        let output = checked(
            execute(cwd, &args, None, false).map_err(|error| io_error(args.join(" "), error))?,
            "rev-parse",
        )?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut lines = text.lines();
        let (Some(_), Some(top_level), Some(git_dir)) = (lines.next(), lines.next(), lines.next())
        else {
            return Err(GitError::new("rev-parse", "unexpected output"));
        };
        Ok(Self {
            top_level: PathBuf::from(top_level.trim()),
            git_dir: PathBuf::from(git_dir.trim()),
        })
    }

    pub fn patch_path(&self) -> PathBuf {
        self.git_dir.join(PATCH_FILE)
    }

    fn raw(&self, args: &[&str], stdin: Option<&[u8]>, literal: bool) -> io::Result<Output> {
        execute(&self.top_level, args, stdin, literal)
    }

    fn git_bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        let output = self
            .raw(args, None, false)
            .map_err(|error| io_error(args.join(" "), error))?;
        Ok(checked(output, args.join(" "))?.stdout)
    }

    pub fn git(&self, args: &[&str]) -> Result<String, GitError> {
        let output = self.git_bytes(args)?;
        Ok(String::from_utf8_lossy(&output).trim_end().to_string())
    }

    pub fn has_initial_commit(&self) -> bool {
        self.git(&["rev-parse", "--verify", "--quiet", "HEAD"])
            .is_ok()
    }

    pub fn staged_files(
        &self,
        diff: Option<&str>,
        filter: &str,
    ) -> Result<Vec<StagedFile>, GitError> {
        let mut args: Vec<String> = vec![String::from("diff"), format!("--diff-filter={filter}")];
        match diff {
            Some(diff) => args.extend(diff.split_whitespace().map(str::to_string)),
            None => args.push(String::from("--staged")),
        }
        args.extend([String::from("--raw"), String::from("-z")]);
        let args = refs(&args);
        let output = self
            .raw(&args, None, false)
            .map_err(|error| io_error(args.join(" "), error))?;
        let output = checked(output, args.join(" "))?;
        Ok(parse_staged(&output.stdout, &self.top_level))
    }

    pub fn partially_staged_files(&self) -> Result<Vec<String>, GitError> {
        let output = self.git_bytes(&["status", "-z"])?;
        let tokens: Vec<&[u8]> = output
            .split(|byte| *byte == 0)
            .filter(|t| !t.is_empty())
            .collect();
        let mut files = Vec::new();
        let mut index = 0;
        while let Some(token) = tokens.get(index) {
            index += 1;
            let (Some(&index_status), Some(&work_status)) = (token.first(), token.get(1)) else {
                continue;
            };
            let staged = STAGED_STATUSES.contains(&index_status);
            let modified = STAGED_STATUSES.contains(&work_status);
            if staged && modified && token.len() > 3 {
                files.push(String::from_utf8_lossy(&token[3..]).into_owned());
            }
            if matches!(index_status, b'R' | b'C') {
                index += 1;
            }
        }
        Ok(files)
    }

    pub fn save_unstaged_patch(&self, files: &[String], output: &Path) -> Result<(), GitError> {
        let mut args: Vec<String> = [
            "diff",
            "--binary",
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--patch",
            "--submodule=short",
            "--output",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
        args.push(output.to_string_lossy().into_owned());
        args.push(String::from("--"));
        args.extend(files.iter().cloned());
        let args = refs(&args);
        let output = self
            .raw(&args, None, true)
            .map_err(|error| io_error(args.join(" "), error))?;
        checked(output, "diff --output <unstaged patch>")?;
        Ok(())
    }

    pub fn checkout_force(&self, files: &[String]) -> Result<(), GitError> {
        self.pathspec_command("checkout", &["--force"], files)
    }

    pub fn add(&self, files: &[String]) -> Result<(), GitError> {
        self.pathspec_command("add", &[], files)
    }

    fn pathspec_command(
        &self,
        command: &str,
        flags: &[&str],
        files: &[String],
    ) -> Result<(), GitError> {
        if files.is_empty() {
            return Ok(());
        }
        let mut args: Vec<String> = vec![command.to_string()];
        args.extend(flags.iter().map(|flag| flag.to_string()));
        args.extend([
            String::from("--pathspec-from-file=-"),
            String::from("--pathspec-file-nul"),
        ]);
        let mut input = Vec::new();
        for file in files {
            input.extend_from_slice(file.as_bytes());
            input.push(0);
        }
        let label = format!("{command} --pathspec-from-file");
        let args = refs(&args);
        let output = self
            .raw(&args, Some(&input), true)
            .map_err(|error| io_error(label.clone(), error))?;
        checked(output, &label)?;
        Ok(())
    }

    pub fn apply_patch(&self, patch: &Path, three_way: bool) -> Result<(), GitError> {
        let patch = patch.to_string_lossy().into_owned();
        let mut args = vec![
            "apply",
            "-v",
            "--whitespace=nowarn",
            "--recount",
            "--unidiff-zero",
        ];
        if three_way {
            args.push("--3way");
        }
        args.push(&patch);
        self.git(&args)?;
        Ok(())
    }

    pub fn create_stash(&self) -> Result<Option<String>, GitError> {
        let hash = self.git(&["stash", "create"])?;
        Ok((!hash.is_empty()).then_some(hash))
    }

    pub fn store_stash(&self, hash: &str) -> Result<(), GitError> {
        self.git(&["stash", "store", "-m", STASH_MESSAGE, hash])?;
        Ok(())
    }

    pub fn stash_index(&self, hash: &str) -> Result<Option<usize>, GitError> {
        let list = self.git(&["stash", "list", "--format=%H"])?;
        Ok(list.lines().position(|line| line.trim() == hash))
    }

    pub fn drop_stash(&self, index: usize) -> Result<(), GitError> {
        let stash = format!("stash@{{{index}}}");
        self.git(&["stash", "drop", "--quiet", &stash])?;
        Ok(())
    }

    pub fn apply_stash(&self, index: usize) -> Result<(), GitError> {
        let stash = format!("stash@{{{index}}}");
        self.git(&["stash", "apply", "--quiet", "--index", &stash])?;
        Ok(())
    }

    pub fn reset_hard(&self) -> Result<(), GitError> {
        self.git(&["reset", "--hard", "HEAD"])?;
        Ok(())
    }

    pub fn config_get(&self, key: &str) -> Result<Option<String>, GitError> {
        let args = ["config", "--get", key];
        let output = self
            .raw(&args, None, false)
            .map_err(|error| io_error(args.join(" "), error))?;
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        let output = checked(output, args.join(" "))?;
        Ok(Some(
            String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_string(),
        ))
    }

    pub fn config_set(&self, key: &str, value: &str) -> Result<(), GitError> {
        self.git(&["config", key, value])?;
        Ok(())
    }

    pub fn config_unset(&self, key: &str) -> Result<(), GitError> {
        let args = ["config", "--unset", key];
        let output = self
            .raw(&args, None, false)
            .map_err(|error| io_error(args.join(" "), error))?;
        if output.status.code() == Some(5) {
            return Ok(());
        }
        checked(output, args.join(" "))?;
        Ok(())
    }
}

fn execute(cwd: &Path, args: &[&str], stdin: Option<&[u8]>, literal: bool) -> io::Result<Output> {
    let mut command = Command::new("git");
    if literal {
        command.arg("--literal-pathspecs");
    }
    command
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    let Some(input) = stdin else {
        return command.stdin(Stdio::null()).output();
    };
    command.stdin(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(input)?;
    }
    child.wait_with_output()
}

fn checked(output: Output, command: impl Into<String>) -> Result<Output, GitError> {
    if output.status.success() {
        return Ok(output);
    }
    let command = command.into();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = match (stderr.is_empty(), stdout.is_empty()) {
        (false, _) => stderr,
        (true, false) => stdout,
        (true, true) => match output.status.code() {
            Some(code) => format!("exited with status {code}"),
            None => String::from("was killed by a signal"),
        },
    };
    Err(GitError::new(command, message))
}

fn parse_staged(stdout: &[u8], top_level: &Path) -> Vec<StagedFile> {
    let tokens: Vec<&[u8]> = stdout
        .split(|byte| *byte == 0)
        .filter(|t| !t.is_empty())
        .collect();
    let mut files = Vec::new();
    let mut index = 0;
    while let Some(info) = tokens.get(index) {
        index += 1;
        if info.first() != Some(&b':') {
            continue;
        }
        let info = String::from_utf8_lossy(info);
        let fields: Vec<&str> = info.split(' ').collect();
        let status = fields.get(4).and_then(|s| s.chars().next()).unwrap_or(' ');
        let Some(path) = tokens.get(index) else {
            break;
        };
        index += 1;
        let renamed = matches!(status, 'R' | 'C');
        let chosen = if renamed {
            match tokens.get(index) {
                Some(new_path) => {
                    index += 1;
                    new_path
                }
                None => path,
            }
        } else {
            path
        };
        let skipped = matches!(fields.get(1), Some(&"160000") | Some(&"120000"));
        if skipped {
            continue;
        }
        files.push(StagedFile {
            path: top_level.join(String::from_utf8_lossy(chosen).as_ref()),
            status,
        });
    }
    files
}

fn refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

fn io_error(command: String, error: io::Error) -> GitError {
    GitError::new(command, error.to_string())
}
