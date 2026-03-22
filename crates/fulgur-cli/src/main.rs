use clap::{Parser, Subcommand};
use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;
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

        /// Output PDF file path (use "-" for stdout)
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

        /// Page margins in mm (CSS shorthand: "20", "20 30", "10 20 30", "10 20 30 40")
        #[arg(long)]
        margin: Option<String>,

        /// Author name (can be specified multiple times)
        #[arg(long = "author")]
        authors: Vec<String>,

        /// Document description
        #[arg(long)]
        description: Option<String>,

        /// Keywords (can be specified multiple times)
        #[arg(long = "keyword")]
        keywords: Vec<String>,

        /// Language code (e.g. ja, en)
        #[arg(long)]
        language: Option<String>,

        /// Creator application name
        #[arg(long)]
        creator: Option<String>,

        /// PDF producer (default: fulgur vX.Y.Z)
        #[arg(long)]
        producer: Option<String>,

        /// Creation date in ISO 8601 format (e.g. 2026-03-22)
        #[arg(long)]
        creation_date: Option<String>,

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

fn parse_margin(s: &str) -> Margin {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let values: Vec<f32> = tokens.iter().filter_map(|v| v.parse().ok()).collect();
    if values.len() != tokens.len() {
        eprintln!(
            "Invalid margin '{}': all values must be numbers (mm). Using default 20mm",
            s
        );
        return Margin::default();
    }
    let to_pt = |mm: f32| mm * 72.0 / 25.4;
    match values.as_slice() {
        [all] => Margin::uniform(to_pt(*all)),
        [vert, horiz] => Margin::symmetric(to_pt(*vert), to_pt(*horiz)),
        [top, horiz, bottom] => Margin {
            top: to_pt(*top),
            right: to_pt(*horiz),
            bottom: to_pt(*bottom),
            left: to_pt(*horiz),
        },
        [top, right, bottom, left] => Margin {
            top: to_pt(*top),
            right: to_pt(*right),
            bottom: to_pt(*bottom),
            left: to_pt(*left),
        },
        _ => {
            eprintln!("Invalid margin '{}', using default 20mm", s);
            Margin::default()
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Render {
            input,
            stdin,
            output,
            size,
            landscape,
            title,
            margin,
            authors,
            description,
            keywords,
            language,
            creator,
            producer,
            creation_date,
            fonts,
            css_files,
        } => {
            let html = if stdin {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                    .expect("Failed to read stdin");
                buf
            } else if let Some(input) = input {
                std::fs::read_to_string(&input).unwrap_or_else(|e| {
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
                .landscape(landscape);

            if let Some(ref m) = margin {
                builder = builder.margin(parse_margin(m));
            }
            if let Some(title) = title {
                builder = builder.title(title);
            }
            if !authors.is_empty() {
                builder = builder.authors(authors);
            }
            if let Some(description) = description {
                builder = builder.description(description);
            }
            if !keywords.is_empty() {
                builder = builder.keywords(keywords);
            }
            if let Some(language) = language {
                builder = builder.lang(language);
            }
            if let Some(creator) = creator {
                builder = builder.creator(creator);
            }
            if let Some(producer) = producer {
                builder = builder.producer(producer);
            }
            if let Some(creation_date) = creation_date {
                builder = builder.creation_date(creation_date);
            }
            if let Some(assets) = assets {
                builder = builder.assets(assets);
            }

            let engine = builder.build();

            if output.as_os_str() == "-" {
                let pdf = engine.render_html(&html).unwrap_or_else(|e| {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                });
                use std::io::Write;
                std::io::stdout().write_all(&pdf).unwrap_or_else(|e| {
                    eprintln!("Error writing to stdout: {e}");
                    std::process::exit(1);
                });
            } else {
                match engine.render_html_to_file(&html, &output) {
                    Ok(()) => eprintln!("PDF written to {}", output.display()),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}
