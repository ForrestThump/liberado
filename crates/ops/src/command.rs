use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<String>,
}

impl CommandSpec {
    pub fn display(&self) -> String {
        let args = self
            .args
            .iter()
            .map(|arg| display_arg(arg))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} {}", self.program, args).trim_end().to_string()
    }

    pub fn run(&self) -> Result<(), String> {
        println!("==> {}", self.label);
        let mut command = liberado_common::process::std_command(&self.program);
        command.args(&self.args);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        if self.stdin.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", self.program))?;
        if let Some(input) = &self.stdin {
            let mut stdin = child.stdin.take().ok_or("child stdin was not piped")?;
            stdin
                .write_all(input.as_bytes())
                .map_err(|error| format!("write stdin for {}: {error}", self.program))?;
        }
        let status = child
            .wait()
            .map_err(|error| format!("wait for {}: {error}", self.program))?;
        if !status.success() {
            return Err(format!("{} failed with {status}", self.display()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandPlan {
    pub steps: Vec<CommandSpec>,
}

impl CommandPlan {
    pub fn print(&self) {
        for (index, step) in self.steps.iter().enumerate() {
            println!("{:>2}. {:<28} {}", index + 1, step.label, step.display());
        }
    }

    pub fn execute(&self) -> Result<(), String> {
        for step in &self.steps {
            step.run()?;
        }
        Ok(())
    }
}

fn display_arg(arg: &str) -> String {
    if arg
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-._/:=@".contains(character))
    {
        arg.to_string()
    } else {
        format!("{arg:?}")
    }
}
