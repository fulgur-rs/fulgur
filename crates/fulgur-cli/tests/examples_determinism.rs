//! Integration test that asserts every bundled example renders to a
//! byte-identical PDF across repeated invocations when the pinned
//! `FONTCONFIG_FILE` from `examples/.fontconfig/fonts.conf` is used.
//!
//! This is the regression harness for `fulgur-a8s` — the determinism
//! caveat around Blitz's global `fontdb` / Parley's system font
//! fallback. If any new example pulls in a glyph that happens to
//! resolve differently under the pinned font set, or if the bundled
//! Noto Sans files drift, this test will catch it locally before CI
//! (or a downstream user) does.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Repository root = two parents above the fulgur-cli crate manifest.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("fulgur-cli crate should be nested under <repo>/crates")
        .to_path_buf()
}

/// Render a single example via the fulgur CLI into `out_path`, with
/// `FONTCONFIG_FILE` pointing at the pinned config.
///
/// Parallel `cargo test` invocations share a single
/// `target/fontconfig-cache` directory — that's intentional.
/// Fontconfig uses atomic rename for its cache writes, so concurrent
/// writers don't corrupt each other. Earlier revisions of this helper
/// tried to scope a per-invocation cache subdirectory, but fontconfig
/// reads the cache location *only* from the `<cachedir>` element in
/// `fonts.conf` (there is no `FC_CACHEDIR` env var), so the per-tag
/// directories were dead code that gave a false sense of isolation.
fn render_example(example_dir: &Path, out_path: &Path) {
    let root = repo_root();
    let html = example_dir.join("index.html");
    assert!(html.exists(), "missing HTML: {}", html.display());

    let fontconfig = root.join("examples/.fontconfig/fonts.conf");
    assert!(
        fontconfig.exists(),
        "missing pinned fontconfig: {}",
        fontconfig.display()
    );

    // Reuse the CLI binary that cargo built for this integration test.
    let fulgur_bin = PathBuf::from(env!("CARGO_BIN_EXE_fulgur"));

    let mut cmd = Command::new(&fulgur_bin);
    cmd.current_dir(&root)
        .env("FONTCONFIG_FILE", &fontconfig)
        .arg("render")
        .arg(&html);

    // Match mise/update-examples.yml behavior for images: register
    // every local image as an asset keyed by its filename. Keep the
    // extension set in sync with the shell scripts in mise.toml,
    // update-examples.yml, and release-prepare.yml — drift here would
    // cause the committed-PDF match check to fail spuriously when a
    // new image format is introduced in one place but not the other.
    //
    // `std::fs::read_dir()` does not guarantee any particular
    // enumeration order (per the stdlib docs), so we collect and
    // sort by path before passing `--image` flags. This keeps the
    // argument order stable across filesystems, reruns, and future
    // libc/toolchain changes, which in turn keeps the emitted PDF
    // byte-for-byte reproducible.
    let mut image_paths: Vec<PathBuf> = std::fs::read_dir(example_dir)
        .expect("readdir example")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let ext = path.extension().and_then(|e| e.to_str())?;
            if matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "gif") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    image_paths.sort();

    for path in &image_paths {
        let filename = path.file_name().and_then(|n| n.to_str()).expect("filename");
        cmd.arg("--image")
            .arg(format!("{}={}", filename, path.display()));
    }

    cmd.arg("-o").arg(out_path);

    let status = cmd.status().expect("spawn fulgur");
    assert!(
        status.success(),
        "fulgur render failed for {}",
        example_dir.display()
    );
}

/// Render an example twice into distinct temp files and assert the
/// outputs are byte-identical. This is the determinism guarantee: two
/// runs, same environment, same bytes.
fn assert_example_deterministic(example_name: &str) {
    let root = repo_root();
    let example_dir = root.join("examples").join(example_name);

    let tmp = tempdir();
    let out_a = tmp.join(format!("{example_name}-a.pdf"));
    let out_b = tmp.join(format!("{example_name}-b.pdf"));

    render_example(&example_dir, &out_a);
    render_example(&example_dir, &out_b);

    let a = std::fs::read(&out_a).expect("read a");
    let b = std::fs::read(&out_b).expect("read b");
    assert_eq!(
        a.len(),
        b.len(),
        "{example_name}: PDF length differs between runs ({} vs {})",
        a.len(),
        b.len()
    );
    assert!(
        a == b,
        "{example_name}: PDFs differ byte-by-byte between runs — determinism broken"
    );
    assert!(a.starts_with(b"%PDF"), "{example_name}: not a valid PDF");
}

/// Minimal tempdir helper — we avoid pulling in a `tempfile` dev-dep
/// just for this test. PID + nanoseconds is plenty for uniqueness
/// across parallel cargo-test workers.
fn tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fulgur-examples-det-{}-{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&dir).expect("mkdir tempdir");
    dir
}

#[test]
fn border_radius_example_is_deterministic() {
    assert_example_deterministic("border-radius");
}

#[test]
fn header_footer_example_is_deterministic() {
    assert_example_deterministic("header-footer");
}

#[test]
fn header_footer_split_example_is_deterministic() {
    assert_example_deterministic("header-footer-split");
}

#[test]
fn image_example_is_deterministic() {
    assert_example_deterministic("image");
}

#[test]
fn link_stylesheet_example_is_deterministic() {
    assert_example_deterministic("link-stylesheet");
}

#[test]
fn svg_example_is_deterministic() {
    // svg is the canonical regression target: before the fontconfig
    // pinning, this example rendered with FreeSans on a dev laptop
    // but DejaVu on CI, producing a 5.7 KB size delta. The test
    // guards against a future silent drift of the same shape.
    assert_example_deterministic("svg");
}

#[test]
fn table_header_example_is_deterministic() {
    assert_example_deterministic("table-header");
}

#[test]
fn text_align_example_is_deterministic() {
    assert_example_deterministic("text-align");
}

#[test]
fn text_decoration_example_is_deterministic() {
    assert_example_deterministic("text-decoration");
}

#[test]
fn pseudo_content_url_example_is_deterministic() {
    // Phase 1 scope (fulgur-ai3): ::before / ::after pseudo elements
    // with `content: url("icon.png"); display: block;`. The icon file
    // must be registered as an asset via `--image`, which the harness
    // does automatically by walking the example directory.
    assert_example_deterministic("pseudo-content-url");
}

/// Cross-check: the committed `examples/<name>/index.pdf` should match
/// what `fulgur render` produces *right now* under the pinned fontconfig.
/// If these drift, either the fonts changed or the code changed and
/// the PDFs are stale — running `mise run update-examples` should fix
/// it, after which the commit lands together with a human review.
///
/// Linux-only: the committed PDFs are regenerated by
/// `.github/workflows/update-examples.yml` on ubuntu-latest, and
/// fontconfig's integration with Parley/usvg differs enough between
/// Linux and macOS/Windows that byte-identity across OSes isn't
/// achievable with the current approach. The same-run determinism
/// guarantee (`assert_example_deterministic` above) still runs on
/// every OS, so within-process regressions are caught everywhere.
#[cfg(target_os = "linux")]
#[test]
fn committed_svg_matches_rendered() {
    let root = repo_root();
    let committed = root.join("examples/svg/index.pdf");
    assert!(
        committed.exists(),
        "committed PDF missing: {}",
        committed.display()
    );

    let tmp = tempdir();
    let out = tmp.join("svg-rendered.pdf");
    render_example(&root.join("examples/svg"), &out);

    let rendered = std::fs::read(&out).expect("read rendered");
    let on_disk = std::fs::read(&committed).expect("read committed");
    assert_eq!(
        rendered.len(),
        on_disk.len(),
        "examples/svg/index.pdf is stale — run `mise run update-examples` \
         to regenerate ({} bytes expected, {} bytes committed)",
        rendered.len(),
        on_disk.len()
    );
    assert!(
        rendered == on_disk,
        "examples/svg/index.pdf is stale — run `mise run update-examples`"
    );
}
