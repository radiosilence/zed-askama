//! `askama-analyzer`: a language server for askama templates, and a `check` command that runs
//! the same analysis from a terminal.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the Language Server Protocol on stdin and stdout. The default.
    Lsp,
    /// Print every template error in the Cargo workspace at PATH, exiting 1 if there are any.
    Check {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Also have this rust-analyzer check the generated code, as the editor does.
        #[arg(long, value_name = "PATH")]
        rust_analyzer: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    // Logs go to stderr: stdout carries the protocol.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("askama_analyzer=info,warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("askama-analyzer: cannot start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    let result = match cli.command.unwrap_or(Command::Lsp) {
        Command::Lsp => runtime.block_on(askama_analyzer::server::run()),
        Command::Check {
            path,
            rust_analyzer,
        } => runtime.block_on(askama_analyzer::check::run(&path, rust_analyzer)),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("askama-analyzer: {err:#}");
            ExitCode::FAILURE
        }
    }
}
