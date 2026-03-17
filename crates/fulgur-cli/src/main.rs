use clap::{Parser, Subcommand};
use fulgur_core::asset::AssetBundle;
use fulgur_core::config::{Margin, PageSize};
use fulgur_core::engine::Engine;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "fulgur", version, about = "HTML to PDF converter")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Render HTML to PDF
    Render {
        /// Input HTML file (omit for --stdin)
        #[arg()]
        input: Option<PathBuf>,

        /// Read HTML from stdin
        #[arg(long)]
        stdin: bool,

        /// Output PDF file path
        #[arg(short, long)]
        output: PathBuf,

        /// Page size (A4, Letter, A3)
        #[arg(short, long, default_value = "A4")]
        size: String,

        /// Landscape orientation
        #[arg(short, long, default_value_t = false)]
        landscape: bool,

        /// PDF title
        #[arg(long)]
        title: Option<String>,

        /// Font files to bundle (can be specified multiple times)
        #[arg(long = "font", short = 'f')]
        fonts: Vec<PathBuf>,

        /// CSS files to include (can be specified multiple times)
        #[arg(long = "css")]
        css_files: Vec<PathBuf>,
    },
}

fn parse_page_size(s: &str) -> PageSize {
    match s.to_uppercase().as_str() {
        "A4" => PageSize::A4,
        "A3" => PageSize::A3,
        "LETTER" => PageSize::LETTER,
        _ => {
            eprintln!("Unknown page size '{}', defaulting to A4", s);
            PageSize::A4
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Render { input, stdin, output, size, landscape, title, fonts, css_files } => {
            let html = if stdin {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                    .expect("Failed to read stdin");
                buf
            } else if let Some(input) = input {
                std::fs::read_to_string(&input)
                    .unwrap_or_else(|e| {
                        eprintln!("Error reading {}: {e}", input.display());
                        std::process::exit(1);
                    })
            } else {
                eprintln!("Error: provide an input HTML file or use --stdin");
                std::process::exit(1);
            };

            // Build assets if fonts or CSS provided
            let assets = if !fonts.is_empty() || !css_files.is_empty() {
                let mut bundle = AssetBundle::new();
                for font_path in &fonts {
                    bundle.add_font_file(font_path).unwrap_or_else(|e| {
                        eprintln!("Warning: failed to load font {}: {e}", font_path.display());
                    });
                }
                for css_path in &css_files {
                    bundle.add_css_file(css_path).unwrap_or_else(|e| {
                        eprintln!("Warning: failed to load CSS {}: {e}", css_path.display());
                    });
                }
                Some(bundle)
            } else {
                None
            };

            let mut builder = Engine::builder()
                .page_size(parse_page_size(&size))
                .margin(Margin::uniform_mm(20.0))
                .landscape(landscape);

            if let Some(title) = title {
                builder = builder.title(title);
            }
            if let Some(assets) = assets {
                builder = builder.assets(assets);
            }

            let engine = builder.build();

            match engine.render_html_to_file(&html, &output) {
                Ok(()) => println!("PDF written to {}", output.display()),
                Err(e) => eprintln!("Error: {e}"),
            }
        }
    }
}
