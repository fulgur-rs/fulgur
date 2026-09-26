//! Development CLI for selecting independently implemented Fulgur backends.

use clap::{Parser, Subcommand, ValueEnum};
use fulgur_core::Result;
use std::path::PathBuf;
use std::process::ExitCode;

mod blitz;

#[derive(Parser)]
#[command(
    name = "fulgur-dev",
    about = "Development CLI for Fulgur layout backends"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Render an HTML file to PDF using the selected backend.
    Render {
        /// Input HTML file.
        input: PathBuf,
        /// Output PDF file.
        #[arg(short, long)]
        output: PathBuf,
        /// Layout backend (Raikiri PDF drawing is not implemented yet).
        #[arg(long, value_enum, default_value = "blitz")]
        engine: EngineChoice,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum EngineChoice {
    Blitz,
    Raikiri,
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Render {
            input,
            output,
            engine,
        } => {
            let bytes = match engine {
                EngineChoice::Blitz => blitz::render(&input),
                EngineChoice::Raikiri => fulgur_raikiri::render(&input),
            }?;
            std::fs::write(output, bytes)?;
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}
