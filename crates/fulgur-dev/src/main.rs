//! Development CLI for selecting independently implemented Fulgur backends.

use clap::{Parser, Subcommand, ValueEnum};
use fulgur_core::{Error, Result};
use std::path::PathBuf;
use std::process::ExitCode;

mod blitz;
mod options;

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
        /// Layout backend.
        #[arg(long, value_enum, default_value = "blitz")]
        engine: EngineChoice,
        /// Paint each page as soon as it is laid out, while the input is
        /// still being read (Raikiri only).
        #[arg(long)]
        stream: bool,
        #[command(flatten)]
        args: options::RenderArgs,
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
            stream,
            args,
        } => {
            let config = args.config();
            config.validate()?;
            let assets = args.assets()?;
            let system_fonts = args.system_fonts();
            let options = fulgur_raikiri::RenderOptions {
                assets: Some(&assets),
                system_fonts,
            };
            let bytes = match (engine, stream) {
                (EngineChoice::Blitz, false) => {
                    blitz::render(&input, &config, Some(&assets), system_fonts)
                }
                (EngineChoice::Blitz, true) => {
                    Err(Error::Other("--stream requires --engine raikiri".into()))
                }
                (EngineChoice::Raikiri, false) => {
                    fulgur_raikiri::render_with_options(&input, &config, &options)
                }
                (EngineChoice::Raikiri, true) => {
                    fulgur_raikiri::render_streaming(&input, &config, &options)
                }
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
