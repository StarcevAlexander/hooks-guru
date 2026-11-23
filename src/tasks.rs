use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl fmt::Display for TaskCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.program)?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub code: Option<i32>,
    pub spawn_error: Option<String>,
    pub output: String,
}

impl CommandResult {
    pub fn success(&self) -> bool {
        self.spawn_error.is_none() && self.code == Some(0)
    }

    pub fn failure(&self) -> Option<String> {
        if let Some(error) = &self.spawn_error {
            return Some(format!("did not start: {error}"));
        }
        match self.code {
            Some(0) => None,
            Some(code) => Some(format!("failed with status {code}")),
            None => Some(String::from("was killed by a signal")),
        }
    }
}

pub trait Executor: Send + Sync {
    fn execute(&self, command: &TaskCommand) -> CommandResult;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessExecutor {
    pub color: bool,
}

impl Executor for ProcessExecutor {
    fn execute(&self, command: &TaskCommand) -> CommandResult {
        let mut process = Command::new(resolve_program(&command.program, &command.cwd));
        process
            .args(&command.args)
            .current_dir(&command.cwd)
            .stdin(Stdio::null())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        if self.color {
            process.env("FORCE_COLOR", "1");
        } else {
            process.env("NO_COLOR", "1");
        }
        match process.output() {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                CommandResult {
                    code: output.status.code(),
                    spawn_error: None,
                    output: text,
                }
            }
            Err(error) => CommandResult {
                code: None,
                spawn_error: Some(error.to_string()),
                output: String::new(),
            },
        }
    }
}

fn is_quote(c: char) -> bool {
    c == '\'' || c == '"'
}

fn closing_quote(chars: &[char], from: usize, quote: char) -> Option<usize> {
    (from..chars.len()).find(|index| chars[*index] == quote)
}

pub fn parse_command(command: &str) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut parts = Vec::new();
    let mut index = 0;
    while let Some(&c) = chars.get(index) {
        if c.is_whitespace() {
            index += 1;
        } else if is_quote(c) {
            match closing_quote(&chars, index + 1, c) {
                Some(close) => {
                    parts.push(chars[index + 1..close].iter().collect());
                    index = close + 1;
                }
                None => index += 1,
            }
        } else {
            let start = index;
            let mut cursor = start + 1;
            loop {
                while chars
                    .get(cursor)
                    .is_some_and(|next| !next.is_whitespace() && !is_quote(*next))
                {
                    cursor += 1;
                }
                let closed = chars
                    .get(cursor)
                    .filter(|next| is_quote(**next))
                    .and_then(|next| closing_quote(&chars, cursor + 1, *next));
                match closed {
                    Some(close) => cursor = close + 1,
                    None => break,
                }
            }
            parts.push(chars[start..cursor].iter().collect());
            index = cursor;
        }
    }
    parts
}

pub fn task_command(command: &str, files: &[String], cwd: &Path) -> TaskCommand {
    let mut parts = parse_command(command).into_iter();
    let program = parts.next().unwrap_or_default();
    let mut args: Vec<String> = parts.collect();
    args.extend_from_slice(files);
    TaskCommand {
        program,
        args,
        cwd: cwd.to_path_buf(),
    }
}

pub fn default_max_arg_length() -> usize {
    if cfg!(target_os = "macos") {
        262_144
    } else if cfg!(windows) {
        8_191
    } else {
        131_072
    }
}

pub fn chunk_files(files: Vec<String>, max_arg_length: Option<usize>) -> Vec<Vec<String>> {
    if files.is_empty() {
        return Vec::new();
    }
    let Some(max) = max_arg_length else {
        return vec![files];
    };
    let total = files.iter().map(String::len).sum::<usize>() + files.len() - 1;
    let count = total.div_ceil(max).clamp(1, files.len());
    let mut chunks = Vec::with_capacity(count);
    let mut position = 0;
    for index in 0..count {
        let length = (files.len() - position).div_ceil(count - index);
        chunks.push(files[position..position + length].to_vec());
        position += length;
    }
    chunks
}

pub fn substitute_hook_args(command: &str, args: &[String]) -> TaskCommand {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut program = String::new();
    let mut command_args = Vec::new();
    for (index, part) in parse_command(command).into_iter().enumerate() {
        match (index, part.as_str()) {
            (0, _) => program = part,
            (_, "{}") => command_args.extend_from_slice(args),
            (_, _) => command_args.push(part),
        }
    }
    TaskCommand {
        program,
        args: command_args,
        cwd,
    }
}

#[cfg(windows)]
pub fn resolve_program(program: &str, cwd: &Path) -> PathBuf {
    let candidate = Path::new(program);
    if program.is_empty()
        || candidate.has_root()
        || candidate.components().count() > 1
        || candidate.extension().is_some()
    {
        return candidate.to_path_buf();
    }
    let extensions =
        std::env::var("PATHEXT").unwrap_or_else(|_| String::from(".COM;.EXE;.BAT;.CMD"));
    let mut dirs = vec![cwd.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    for dir in dirs {
        for extension in extensions.split(';').filter(|ext| !ext.is_empty()) {
            let full = dir.join(format!("{program}{extension}"));
            if full.is_file() {
                return full;
            }
        }
    }
    candidate.to_path_buf()
}

#[cfg(not(windows))]
pub fn resolve_program(program: &str, _cwd: &Path) -> PathBuf {
    PathBuf::from(program)
}

pub fn io_error(error: impl fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

pub fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else {
        format!("{:.2}s", duration.as_secs_f64())
    }
}
