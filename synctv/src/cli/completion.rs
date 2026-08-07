use std::io::{self, Write};

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, ValueEnum};

use super::Cli;

#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: CompletionShell,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

pub(super) fn execute_completion(args: &CompletionArgs) -> Result<()> {
    let mut command = Cli::command();
    match args.shell {
        CompletionShell::Bash => print_completion(&mut command, clap_complete::Shell::Bash)?,
        CompletionShell::Zsh => print_completion(&mut command, clap_complete::Shell::Zsh)?,
        CompletionShell::Fish => print_completion(&mut command, clap_complete::Shell::Fish)?,
        CompletionShell::PowerShell => {
            print_completion(&mut command, clap_complete::Shell::PowerShell)?;
        }
        CompletionShell::Elvish => print_completion(&mut command, clap_complete::Shell::Elvish)?,
    }
    Ok(())
}

fn print_completion(command: &mut clap::Command, shell: clap_complete::Shell) -> Result<()> {
    let mut buffer = Vec::new();
    clap_complete::generate(shell, command, "synctv", &mut buffer);

    let mut stdout = io::stdout().lock();
    write_completion_output(&mut stdout, &buffer)
}

#[cfg(test)]
pub(super) fn write_completion_output<W: Write>(writer: &mut W, buffer: &[u8]) -> Result<()> {
    write_completion_output_inner(writer, buffer)
}

#[cfg(not(test))]
fn write_completion_output<W: Write>(writer: &mut W, buffer: &[u8]) -> Result<()> {
    write_completion_output_inner(writer, buffer)
}

fn write_completion_output_inner<W: Write>(writer: &mut W, buffer: &[u8]) -> Result<()> {
    match writer.write_all(buffer).and_then(|()| writer.flush()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("failed to write shell completion output"),
    }
}
