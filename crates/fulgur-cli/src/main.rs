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

        /// Image files to bundle (name=path, can be specified multiple times)
        /// Example: --image logo.png=assets/logo.png
        #[arg(long = "image", short = 'i')]
        images: Vec<String>,

        /// JSON data file for template mode (use "-" for stdin)
        #[arg(long = "data", short = 'd')]
        data: Option<PathBuf>,
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
            images,
            data,
        } => {
            if stdin && data.as_ref().is_some_and(|p| p.as_os_str() == "-") {
                eprintln!("Error: cannot use --stdin and --data - together (both read stdin)");
                std::process::exit(1);
            }

            // Compute base_path before consuming input
            let base_path = if stdin {
                std::env::current_dir().ok()
            } else {
                input.as_ref().and_then(|p| {
                    p.canonicalize()
                        .ok()
                        .and_then(|abs| abs.parent().map(|d| d.to_path_buf()))
                        .or_else(|| {
                            p.parent()
                                .map(|d| d.to_path_buf())
                                .filter(|d| !d.as_os_str().is_empty())
                        })
                        .or_else(|| std::env::current_dir().ok())
                })
            };

            let input_content = if stdin {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                    .expect("Failed to read stdin");
                buf
            } else if let Some(ref input) = input {
                std::fs::read_to_string(input).unwrap_or_else(|e| {
                    eprintln!("Error reading {}: {e}", input.display());
                    std::process::exit(1);
                })
            } else {
                eprintln!("Error: provide an input HTML file or use --stdin");
                std::process::exit(1);
            };

            // Build assets if fonts, CSS, or images provided
            let assets = if !fonts.is_empty() || !css_files.is_empty() || !images.is_empty() {
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
                for image_spec in &images {
                    if let Some((name, path)) = image_spec.split_once('=') {
                        bundle.add_image_file(name, path).unwrap_or_else(|e| {
                            eprintln!("Warning: failed to load image {}: {e}", path);
                        });
                    } else {
                        // Use filename as the image name
                        let path = std::path::Path::new(image_spec);
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(image_spec);
                        bundle.add_image_file(name, path).unwrap_or_else(|e| {
                            eprintln!("Warning: failed to load image {}: {e}", image_spec);
                        });
                    }
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
            if let Some(ref base_path) = base_path {
                builder = builder.base_path(base_path);
            }
            if let Some(assets) = assets {
                builder = builder.assets(assets);
            }

            // Template mode: add template and data to builder
            if let Some(ref data_path) = data {
                let json_str = if data_path.as_os_str() == "-" {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                        .expect("Failed to read JSON from stdin");
                    buf
                } else {
                    std::fs::read_to_string(data_path).unwrap_or_else(|e| {
                        eprintln!("Error reading data file {}: {e}", data_path.display());
                        std::process::exit(1);
                    })
                };
                let json_data: serde_json::Value =
                    serde_json::from_str(&json_str).unwrap_or_else(|e| {
                        eprintln!("Error parsing JSON: {e}");
                        std::process::exit(1);
                    });
                let template_name = input
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("template.html");
                builder = builder
                    .template(template_name, &input_content)
                    .data(json_data);
            }

            let engine = builder.build();

            let pdf = if data.is_some() {
                engine.render()
            } else {
                engine.render_html(&input_content)
            }
            .unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            if output.as_os_str() == "-" {
                use std::io::Write;
                std::io::stdout().write_all(&pdf).unwrap_or_else(|e| {
                    eprintln!("Error writing to stdout: {e}");
                    std::process::exit(1);
                });
            } else {
                std::fs::write(&output, &pdf).unwrap_or_else(|e| {
                    eprintln!("Error writing to {}: {e}", output.display());
                    std::process::exit(1);
                });
                eprintln!("PDF written to {}", output.display());
            }
        }
    }
}
