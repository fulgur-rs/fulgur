//! Thin adapter over Blitz APIs. All Blitz-specific code is isolated here
//! so that upstream API changes only require changes in this module.

use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};
use parley::FontContext;
use std::sync::Arc;

/// Suppress stdout during a closure. Blitz's HTML parser unconditionally prints
/// `println!("ERROR: {error}")` for non-fatal parse errors (e.g., "Unexpected token").
/// These are html5ever's error-recovery messages and do not indicate real failures.
fn suppress_stdout<F: FnOnce() -> T, T>(f: F) -> T {
    use std::io::Write;

    // Flush any pending stdout first
    let _ = std::io::stdout().flush();

    // On Unix, redirect fd 1 to /dev/null temporarily
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let devnull = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .ok();
        let saved_fd = devnull.as_ref().map(|_| {
            // dup(1) to save original stdout
            let saved = unsafe { libc::dup(1) };
            if saved < 0 {
                return -1;
            }
            // dup2(devnull_fd, 1) to redirect stdout
            if let Some(ref dn) = devnull {
                unsafe { libc::dup2(dn.as_raw_fd(), 1) };
            }
            saved
        });

        let result = f();

        // Restore original stdout
        if let Some(Some(saved)) = saved_fd.map(|fd| if fd >= 0 { Some(fd) } else { None }) {
            let _ = std::io::stdout().flush();
            unsafe { libc::dup2(saved, 1) };
            unsafe { libc::close(saved) };
        }

        result
    }

    #[cfg(not(unix))]
    {
        f()
    }
}

/// Parse HTML and return a fully resolved document (styles + layout computed).
///
/// We pass the content width as the viewport width so Taffy wraps text
/// at the right column. The viewport height is set very large so that
/// Taffy lays out the full document without clipping — our own pagination
/// algorithm handles page breaks.
pub fn parse_and_layout(
    html: &str,
    viewport_width: f32,
    _viewport_height: f32,
    font_data: &[Arc<Vec<u8>>],
) -> HtmlDocument {
    let viewport = Viewport::new(
        viewport_width as u32,
        10000, // Large height — let Taffy lay out everything, we paginate later
        1.0,
        ColorScheme::Light,
    );

    // Build FontContext with bundled fonts
    let font_ctx = if font_data.is_empty() {
        None
    } else {
        let mut ctx = FontContext::new();
        for data in font_data {
            let blob: parley::fontique::Blob<u8> = (**data).clone().into();
            ctx.collection.register_fonts(blob, None);
        }
        Some(ctx)
    };

    let config = DocumentConfig {
        viewport: Some(viewport),
        font_ctx,
        ..DocumentConfig::default()
    };

    // Suppress Blitz's noisy "ERROR: Unexpected token" println output
    let mut doc = suppress_stdout(|| HtmlDocument::from_html(html, config));

    // Resolve styles (Stylo) and layout (Taffy)
    doc.resolve(0.0);

    doc
}
