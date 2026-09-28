use crate::{Result, invalid};
use std::{
    io::Write,
    process::{Command, Stdio},
};

/// Runs an external command with piped standard input and captures its output.
pub fn execute(command: &mut Command, input: &[u8]) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let write = child
        .stdin
        .take()
        .ok_or_else(|| invalid("Missing process input"))?
        .write_all(input);
    // Reap the child even if writing to its stdin fails, then report process failure first.
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(invalid(format!(
            "Process failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    write?;
    Ok(result.stdout)
}

/// Abstracts process execution so connection workflows can be tested without a device.
pub trait Runner {
    /// Executes a command with the supplied standard input and returns standard output.
    fn run(&self, command: &mut Command, input: &[u8]) -> Result<Vec<u8>>;
}

/// Executes commands using the local operating system.
pub struct SystemRunner;

impl Runner for SystemRunner {
    // Uses the same process handling as direct client commands.
    fn run(&self, command: &mut Command, input: &[u8]) -> Result<Vec<u8>> {
        execute(command, input)
    }
}
