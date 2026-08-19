use clap::{Parser, Subcommand};
use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;
use std::ffi::OsString;
use std::path::PathBuf;

mod plugin;

/// Isolate the real stdout from noise emitted by the render pipeline so that
/// PDF bytes written to stdout (`-o -`) cannot be corrupted by incidental
/// output from dependencies.
///
/// `blitz-html` prints `println!("ERROR: {msg}")` for every non-fatal
/// html5ever parse error in its `TreeSink::finish` implementation. When fulgur
/// renders to stdout, those messages would be written before the PDF bytes,
/// producing a file that does not start with `%PDF-` and is therefore invalid.
///
/// The isolator duplicates fd 1 (real stdout), remaps fd 1 to fd 2 (stderr)
/// so any stray `println!` goes to stderr where the user can see it, and
/// writes PDF bytes directly to the saved fd via `libc::write`. On Drop the
/// real stdout is restored.
///
/// This is safe because the CLI is strictly single-threaded during the render
/// phase — unlike the previous `suppress_stdout` helper inside `blitz_adapter`
/// which was shared by multi-threaded library callers and had a race in its
/// `dup2` manipulation (see
/// `docs/plans/2026-04-11-blitz-thread-safety-investigation.md`).
#[cfg(unix)]
struct StdoutIsolator {
    saved_fd: libc::c_int,
}

#[cfg(unix)]
impl StdoutIsolator {
    /// Install the isolator. Returns `None` if either `dup(1)` or `dup2(2, 1)`
    /// fails, in which case the caller should fall back to an unisolated
    /// write.
    fn install() -> Option<Self> {
        use std::io::Write;
        // Flush any pending stdout buffers before redirecting so nothing is
        // left in std's userland buffer pointing at the old fd.
        let _ = std::io::stdout().flush();

        let saved = unsafe { libc::dup(1) };
        if saved < 0 {
            return None;
        }
        if unsafe { libc::dup2(2, 1) } < 0 {
            unsafe { libc::close(saved) };
            return None;
        }
        Some(Self { saved_fd: saved })
    }

    /// Write the given bytes to the saved real stdout fd, bypassing the
    /// process-wide fd 1 (which is currently pointing at stderr).
    ///
    /// Retries on `EINTR` to match the semantics of
    /// `std::io::Write::write_all`, which we are replacing at the syscall
    /// level. Without this, a signal delivered mid-write (e.g. `SIGWINCH`
    /// on terminal resize, `SIGCHLD` from a child process, or a timer
    /// signal) would surface as a spurious `Interrupted system call`
    /// failure. A `0`-byte return on a non-empty buffer is treated as
    /// `WriteZero` to prevent an infinite loop.
    fn write_all(&self, mut data: &[u8]) -> std::io::Result<()> {
        while !data.is_empty() {
            let written = unsafe {
                libc::write(
                    self.saved_fd,
                    data.as_ptr() as *const libc::c_void,
                    data.len(),
                )
            };
            if written < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if written == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "libc::write returned 0 bytes for a non-empty buffer",
                ));
            }
            data = &data[written as usize..];
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for StdoutIsolator {
    fn drop(&mut self) {
        // Flush any bytes still sitting in Rust's `io::Stdout` buffer while
        // fd 1 is still pointing at stderr. `io::Stdout` is a LineWriter that
        // flushes on newline, so the common case of `println!("ERROR: ...")`
        // from dependencies is already flushed inline. This flush is
        // defense-in-depth for writes without a trailing newline (e.g. a
        // future dependency using `print!`, or blitz changing its error
        // sink), and keeps the Drop symmetric with `install()` which also
        // flushes before manipulating fd 1.
        use std::io::Write;
        let _ = std::io::stdout().flush();
        // Restore fd 1 to the real stdout so any final messages the process
        // prints (e.g. panic output on failure paths) land in the right place.
        unsafe {
            libc::dup2(self.saved_fd, 1);
            libc::close(self.saved_fd);
        }
    }
}

#[derive(Parser)]
#[command(name = "fulgur", version, about = "HTML to PDF converter")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Render HTML to PDF
    #[command(after_long_help = "\
\x1b[1;4mTemplate filters:\x1b[0m

  When using --data, the input HTML is processed as a MiniJinja template.
  The following filters are available:

  \x1b[1mBuilt-in filters (MiniJinja):\x1b[0m
    String:  upper, lower, title, capitalize, trim, replace, split, lines
    List:    first, last, length, reverse, sort, unique, join, slice, batch
    Select:  select, reject, selectattr, rejectattr, map, groupby, chain, zip
    Dict:    items, dictsort, attr
    Type:    int, float, bool, string, list, abs, round, sum, min, max
    Format:  format (printf-style), tojson, pprint, urlencode, indent
    Other:   default (d), safe, escape (e)

  \x1b[1mCustom filters:\x1b[0m
    numformat(spec)  Python-style numeric formatting
      {{ price | numformat(\",\") }}      → 1,234,567       (comma separator)
      {{ price | numformat(\",.2f\") }}   → 1,234,567.89    (comma + 2 decimals)
      {{ rate  | numformat(\".2f\") }}    → 10.50            (2 decimal places)
      {{ seq   | numformat(\"04d\") }}    → 0005             (zero-padded)
")]
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

        /// Page size: keyword (A4, Letter, A3) or custom WxH with units
        /// (units mm/cm/in/pt/px; 'x' or space separator),
        /// e.g. 210x297mm or 2352.6ptx3481.39pt.
        /// Takes priority over CSS @page { size }, including orientation:
        /// with --size, CSS @page landscape/portrait is ignored and the page
        /// keeps the orientation implied by the size itself (its own W×H, or a
        /// keyword's portrait default) unless --landscape swaps it. Omit --size
        /// to let CSS @page { size } take effect (falls back to A4 if neither set).
        #[arg(short, long)]
        size: Option<String>,

        /// Landscape orientation (also forces landscape over CSS @page)
        #[arg(short, long)]
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

        /// PDF producer (default: fulgur)
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

        /// MiniJinja JSON data file for template rendering ("-" for stdin, see `fulgur template`)
        #[arg(long = "data", short = 'd')]
        data: Option<PathBuf>,

        /// Generate PDF bookmarks (outline) from h1-h6 headings.
        #[arg(long)]
        bookmarks: bool,

        /// Enable Tagged PDF output (structure tree).
        #[arg(long)]
        tagged: bool,

        /// Enable PDF/UA-1 conformance (implies --tagged and --bookmarks).
        #[arg(long = "pdf-ua")]
        pdf_ua: bool,
    },
    /// Inspect a PDF and extract text positions, images, and metadata as JSON
    Inspect {
        /// Input PDF file
        #[arg()]
        input: PathBuf,

        /// Output JSON file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Template utilities (powered by MiniJinja)
    Template {
        #[command(subcommand)]
        command: TemplateCommands,
    },
    /// List discovered plugins on `$PATH`.
    Plugins,
    /// External plugin: dispatches to `fulgur-<name>` on `$PATH`.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Subcommand)]
enum TemplateCommands {
    /// Extract JSON Schema from a MiniJinja HTML template.
    /// Analyzes template syntax to infer variable names and types.
    /// With --data, uses actual JSON values for precise type inference.
    Schema {
        /// Input HTML template file
        #[arg()]
        input: PathBuf,

        /// Sample JSON data file for precise type inference (use "-" for stdin)
        #[arg(long = "data", short = 'd')]
        data: Option<PathBuf>,

        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn parse_page_size(s: &str) -> PageSize {
    let s = s.trim();
    match s.to_uppercase().as_str() {
        "A4" => PageSize::A4,
        "A3" => PageSize::A3,
        "LETTER" => PageSize::LETTER,
        _ => parse_custom_size(s).unwrap_or_else(|| {
            eprintln!(
                "Unknown page size '{}', defaulting to A4. \
                 Use a keyword (A4, Letter, A3) or custom WxH with units \
                 (e.g. 210x297mm, 2352.6ptx3481.39pt; units mm/cm/in/pt/px, \
                 'x' or space separator), or set the size via CSS \
                 @page {{ size }} and omit --size.",
                s
            );
            PageSize::A4
        }),
    }
}

/// Known absolute CSS length units accepted for `--size`. All are two ASCII
/// bytes and none is a prefix of another, so match order does not matter.
/// The disambiguation that matters is *positional*: the scanner matches a
/// unit before it tests for a separator, so the `x` in `px` is consumed as
/// part of the unit rather than mistaken for the `x` separator.
const PAGE_UNITS: [&str; 5] = ["mm", "cm", "in", "pt", "px"];

/// Convert an absolute length value + unit to PDF points.
/// Mirrors `fulgur::gcpm::parser::css_unit_to_pt` (private there; the
/// five-line table is duplicated locally rather than widening fulgur's
/// public API).
fn unit_to_pt(value: f32, unit: &str) -> Option<f32> {
    let factor = match () {
        _ if unit.eq_ignore_ascii_case("mm") => 72.0 / 25.4,
        _ if unit.eq_ignore_ascii_case("cm") => 72.0 / 2.54,
        _ if unit.eq_ignore_ascii_case("in") => 72.0,
        _ if unit.eq_ignore_ascii_case("pt") => 1.0,
        _ if unit.eq_ignore_ascii_case("px") => 72.0 / 96.0,
        _ => return None,
    };
    Some(value * factor)
}

/// Consume a leading `[0-9.]+` run and parse it as a non-negative f32.
/// (A leading `-` is not consumed, so the value is never negative; the
/// positive-value guard lives at the `parse_custom_size` call site.)
fn take_number(rest: &str) -> Option<(f32, &str)> {
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let (num, tail) = rest.split_at(end);
    let v: f32 = num.parse().ok()?;
    Some((v, tail))
}

/// Greedily consume a known page unit at the start of `rest`.
/// Returns `(Some(unit), tail)` or `(None, rest)` when none matches.
fn take_unit(rest: &str) -> (Option<&str>, &str) {
    for u in PAGE_UNITS {
        if let Some(head) = rest.get(..u.len())
            && head.eq_ignore_ascii_case(u)
        {
            return (Some(head), &rest[u.len()..]);
        }
    }
    (None, rest)
}

/// Parse `WxH` custom page dimensions, e.g. `210x297mm` or `2352.6ptx3481.39pt`.
/// Separator is an `x`/`X` (optionally surrounded by whitespace) or a bare run
/// of whitespace. A unit on one side applies to both; both sides unitless is
/// rejected (unit required).
fn parse_custom_size(s: &str) -> Option<PageSize> {
    let s = s.trim();

    let (wv, rest) = take_number(s)?;
    let (wunit, rest) = take_unit(rest);

    // Separator: optional whitespace, then 'x'/'X', then optional whitespace;
    // or a bare run of whitespace with no 'x' (e.g. "100mm 200mm").
    let after_lead_ws = rest.trim_start();
    let rest = if let Some(after) = after_lead_ws.strip_prefix(['x', 'X']) {
        after.trim_start()
    } else if after_lead_ws.len() != rest.len() {
        after_lead_ws // whitespace-only separator
    } else {
        return None; // no separator
    };

    let (hv, rest) = take_number(rest)?;
    let (hunit, rest) = take_unit(rest);
    if !rest.trim().is_empty() {
        return None; // trailing garbage
    }

    // A unit on one side applies to both; both missing is invalid.
    let (wu, hu) = match (wunit, hunit) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, a),
        (None, Some(b)) => (b, b),
        (None, None) => return None,
    };

    let width = unit_to_pt(wv, wu)?;
    let height = unit_to_pt(hv, hu)?;
    if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
        Some(PageSize { width, height })
    } else {
        None
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
    // `f32::from_str` accepts "nan"/"inf"/"-inf" as valid numbers, and a
    // bare negative value parses fine syntactically — neither is a usable
    // margin, so reject explicitly rather than let `Engine::render` reject
    // it later with a less specific error.
    if values.iter().any(|v| !v.is_finite() || *v < 0.0) {
        eprintln!(
            "Invalid margin '{}': all values must be finite and non-negative (mm). Using default 20mm",
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

/// Minimal `log` sink that writes to **stderr**.
///
/// `crates/fulgur` reports non-fatal problems through the `log` facade —
/// missing assets (`asset.rs`), unparseable CSS (`column_css.rs`), and
/// fragments placed past the page box (`pagination_layout.rs`). A
/// facade with no installed logger silently drops every one of them, so
/// until this existed the CLI showed the user nothing: a render that
/// paints content off the paper and loses it exits `0` with no output.
///
/// stderr, not stdout: `-o -` writes PDF bytes to stdout and any other
/// byte there corrupts the stream (see [`StdoutIsolator`]).
///
/// Deliberately not `env_logger` — that pulls a regex / ANSI stack in
/// for what is one `writeln!`, and `RUST_LOG=<level>` covers the whole
/// need here. Unrecognised values fall back to the default rather than
/// erroring, since logging configuration must never fail a render.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            // `eprintln!` panics on a write failure (e.g. a broken pipe);
            // a logging failure must never abort an otherwise-successful
            // render (CodeRabbit review, PR #719), so write directly and
            // ignore the `Result`.
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "{}: {}",
                record.level().as_str().to_lowercase(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

/// Install [`StderrLogger`], honouring `RUST_LOG` (default: `warn`).
fn init_logging() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| v.trim().parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Warn);
    // Only fails if a logger is already installed, which cannot happen
    // here — but a logging failure must never abort a render.
    if log::set_logger(&StderrLogger).is_ok() {
        log::set_max_level(level);
    }
}

fn main() {
    init_logging();
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
            bookmarks,
            tagged,
            pdf_ua,
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

            let mut builder = Engine::builder();
            if let Some(ref s) = size {
                builder = builder.page_size(parse_page_size(s));
            }
            if landscape {
                builder = builder.landscape(landscape);
            }

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
            builder = builder.bookmarks(bookmarks);
            builder = builder.tagged(tagged);
            builder = builder.pdf_ua(pdf_ua);
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

            // Isolate stdout BEFORE rendering for both output modes: blitz-html
            // prints `println!("ERROR: {e}")` for every non-fatal html5ever
            // parse-error recovery, and we want those on stderr where they
            // belong (a) so they don't corrupt the PDF stream in `-o -` mode,
            // and (b) so they don't pollute the user's terminal stdout in
            // file-output mode. The isolator redirects fd 1 -> fd 2 for the
            // duration of the render.
            //
            // Install failure handling depends on the output mode:
            // * `-o -` (writing PDF to stdout) MUST have the isolator — a
            //   failure would leave dependency noise free to corrupt the PDF
            //   bytes. Abort with a clear error so the user can investigate.
            // * File output can tolerate an install failure: the worst case
            //   is blitz parse-error lines leaking to the user's terminal,
            //   which is UX noise but not a correctness bug.
            let to_stdout = output.as_os_str() == "-";
            #[cfg(unix)]
            let stdout_isolator = {
                let iso = StdoutIsolator::install();
                if to_stdout && iso.is_none() {
                    eprintln!(
                        "Error: failed to isolate stdout for `-o -` output. \
                         Refusing to write PDF bytes without protection — \
                         dependency output could corrupt the stream. \
                         Retry with `-o <file>` or investigate the environment \
                         (fd 1 closed? per-process fd limit reached?)."
                    );
                    std::process::exit(1);
                }
                iso
            };

            let pdf = if data.is_some() {
                engine.render_template()
            } else {
                engine.render(&input_content)
            }
            .unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            if to_stdout {
                #[cfg(unix)]
                {
                    // Install is verified above for `-o -` mode, so the
                    // isolator is guaranteed to be Some here.
                    let iso = stdout_isolator
                        .as_ref()
                        .expect("isolator install verified non-None for -o -");
                    iso.write_all(&pdf).unwrap_or_else(|e| {
                        eprintln!("Error writing to stdout: {e}");
                        std::process::exit(1);
                    });
                }
                #[cfg(not(unix))]
                {
                    // Non-unix build: no StdoutIsolator available. Dependency
                    // output may interleave with the PDF bytes; the Unix path
                    // is the supported configuration for `-o -`.
                    use std::io::Write;
                    std::io::stdout().write_all(&pdf).unwrap_or_else(|e| {
                        eprintln!("Error writing to stdout: {e}");
                        std::process::exit(1);
                    });
                }
            } else {
                std::fs::write(&output, &pdf).unwrap_or_else(|e| {
                    eprintln!("Error writing to {}: {e}", output.display());
                    std::process::exit(1);
                });
                eprintln!("PDF written to {}", output.display());
            }
        }
        Commands::Inspect { input, output } => {
            let result = fulgur::inspect::inspect(&input).unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });
            let json = serde_json::to_string_pretty(&result).unwrap_or_else(|e| {
                eprintln!("Error serializing JSON: {e}");
                std::process::exit(1);
            });
            if let Some(ref output_path) = output {
                std::fs::write(output_path, &json).unwrap_or_else(|e| {
                    eprintln!("Error writing to {}: {e}", output_path.display());
                    std::process::exit(1);
                });
                eprintln!("Inspection JSON written to {}", output_path.display());
            } else {
                println!("{json}");
            }
        }
        Commands::Template { command } => match command {
            TemplateCommands::Schema {
                input,
                data,
                output,
            } => {
                let template_str = std::fs::read_to_string(&input).unwrap_or_else(|e| {
                    eprintln!("Error reading {}: {e}", input.display());
                    std::process::exit(1);
                });
                let template_name = input
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("template.html");

                let schema = if let Some(ref data_path) = data {
                    let json_str = if data_path.as_os_str() == "-" {
                        let mut buf = String::new();
                        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                            .expect("Failed to read JSON from stdin");
                        buf
                    } else {
                        std::fs::read_to_string(data_path).unwrap_or_else(|e| {
                            eprintln!("Error reading {}: {e}", data_path.display());
                            std::process::exit(1);
                        })
                    };
                    let json_data: serde_json::Value = serde_json::from_str(&json_str)
                        .unwrap_or_else(|e| {
                            eprintln!("Error parsing JSON: {e}");
                            std::process::exit(1);
                        });
                    fulgur::schema::extract_schema_with_data(
                        &template_str,
                        template_name,
                        &json_data,
                    )
                } else {
                    fulgur::schema::extract_schema(&template_str, template_name)
                }
                .unwrap_or_else(|e| {
                    eprintln!("Error extracting schema: {e}");
                    std::process::exit(1);
                });

                let json_output = serde_json::to_string_pretty(&schema).unwrap();

                if let Some(ref output_path) = output {
                    std::fs::write(output_path, &json_output).unwrap_or_else(|e| {
                        eprintln!("Error writing to {}: {e}", output_path.display());
                        std::process::exit(1);
                    });
                    eprintln!("Schema written to {}", output_path.display());
                } else {
                    println!("{json_output}");
                }
            }
        },
        Commands::Plugins => {
            const BUILTINS: &[&str] = &["render", "inspect", "template", "plugins"];
            let entries = plugin::list();
            if entries.is_empty() {
                eprintln!("No fulgur plugins found on $PATH.");
                return;
            }
            println!("Available plugins (from $PATH):");
            for entry in entries {
                let suffix = if BUILTINS.contains(&entry.name.as_str()) {
                    "  (shadowed by built-in)"
                } else if entry.shadowed {
                    "  (shadowed)"
                } else {
                    ""
                };
                println!(
                    "  fulgur-{:<12} {}{}",
                    entry.name,
                    entry.path.display(),
                    suffix
                );
            }
        }
        Commands::External(args) => {
            plugin::dispatch(args);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn keyword_still_works() {
        assert!(approx(parse_page_size("A4").width, PageSize::A4.width));
        assert!(approx(
            parse_page_size("letter").height,
            PageSize::LETTER.height
        ));
        assert!(approx(parse_page_size("A3").width, PageSize::A3.width));
    }

    #[test]
    fn custom_pt_both_units() {
        let s = parse_page_size("2352.6ptx3481.39pt");
        assert!(approx(s.width, 2352.6));
        assert!(approx(s.height, 3481.39));
    }

    #[test]
    fn custom_mm_trailing_unit_applies_to_both() {
        // 100mm = 283.46pt, 200mm = 566.93pt (distinct from the A4 fallback)
        let s = parse_page_size("100x200mm");
        assert!(approx(s.width, 100.0 * 72.0 / 25.4));
        assert!(approx(s.height, 200.0 * 72.0 / 25.4));
    }

    #[test]
    fn custom_leading_unit_applies_to_both() {
        let s = parse_page_size("100mmx200");
        assert!(approx(s.width, 100.0 * 72.0 / 25.4));
        assert!(approx(s.height, 200.0 * 72.0 / 25.4));
    }

    #[test]
    fn custom_px_separator_ambiguity() {
        // px は x を含む。素朴な split では壊れるケース。800px=600pt, 1200px=900pt
        let s = parse_page_size("800pxx1200px");
        assert!(approx(s.width, 600.0));
        assert!(approx(s.height, 900.0));
    }

    #[test]
    fn custom_whitespace_separator() {
        let s = parse_page_size("100mm 200mm");
        assert!(approx(s.width, 100.0 * 72.0 / 25.4));
        assert!(approx(s.height, 200.0 * 72.0 / 25.4));
    }

    #[test]
    fn custom_whitespace_around_x_separator() {
        // 'x' may be surrounded by whitespace: "100mm x 200mm".
        let s = parse_page_size("100mm x 200mm");
        assert!(approx(s.width, 100.0 * 72.0 / 25.4));
        assert!(approx(s.height, 200.0 * 72.0 / 25.4));
        // Asymmetric spacing is accepted too.
        let s = parse_page_size("100mmx 200mm");
        assert!(approx(s.width, 100.0 * 72.0 / 25.4));
        let s = parse_page_size("100mm x200mm");
        assert!(approx(s.height, 200.0 * 72.0 / 25.4));
    }

    #[test]
    fn custom_mixed_units() {
        // 100mm x 2in = 283.46pt x 144pt
        let s = parse_page_size("100mmx2in");
        assert!(approx(s.width, 100.0 * 72.0 / 25.4));
        assert!(approx(s.height, 144.0));
    }

    #[test]
    fn unitless_falls_back_to_a4() {
        // 単位必須: 両側単位なしは不正 → A4
        assert!(approx(
            parse_page_size("800x1200").width,
            PageSize::A4.width
        ));
    }

    #[test]
    fn garbage_falls_back_to_a4() {
        assert!(approx(parse_page_size("banana").width, PageSize::A4.width));
        assert!(approx(parse_page_size("100mm").width, PageSize::A4.width)); // 単一値
        assert!(approx(
            parse_page_size("100foox200bar").width,
            PageSize::A4.width
        )); // 未知単位
        assert!(approx(
            parse_page_size("0ptx100pt").width,
            PageSize::A4.width
        )); // 非正値
    }

    #[test]
    fn margin_valid_uniform() {
        let m = parse_margin("20");
        assert!(approx(m.top, 20.0 * 72.0 / 25.4));
        assert_eq!(m.top, m.right);
        assert_eq!(m.top, m.bottom);
        assert_eq!(m.top, m.left);
    }

    #[test]
    fn margin_rejects_negative_falls_back_to_default() {
        let m = parse_margin("-5");
        assert_eq!(m, Margin::default());
    }

    #[test]
    fn margin_rejects_nan_falls_back_to_default() {
        // Rust's f32::from_str accepts "nan"/"inf"/"-inf" as valid numbers,
        // so these parse successfully as tokens and must be caught by the
        // finite check rather than the "not a number" branch.
        let m = parse_margin("nan");
        assert_eq!(m, Margin::default());
    }

    #[test]
    fn margin_rejects_infinite_falls_back_to_default() {
        let m = parse_margin("inf");
        assert_eq!(m, Margin::default());
        let m = parse_margin("-inf");
        assert_eq!(m, Margin::default());
    }

    #[test]
    fn margin_rejects_one_bad_value_among_four() {
        let m = parse_margin("10 10 10 -10");
        assert_eq!(m, Margin::default());
    }
}
