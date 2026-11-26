//! Compare the public PDF entry points using a single explicitly supplied font.
//! Usage: table_probe INPUT.html OUTPUT.pdf FONT [batch|streaming]

use std::{env, ffi::OsStr, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let [input, output, font, mode @ ..] = args.as_slice() else {
        return Err("usage: table_probe INPUT.html OUTPUT.pdf FONT [batch|streaming]".into());
    };
    let render = match mode {
        [] => fulgur_raikiri::render_streaming,
        [name] if name == OsStr::new("streaming") => fulgur_raikiri::render_streaming,
        [name] if name == OsStr::new("batch") => fulgur_raikiri::render_with_options,
        _ => return Err("expected one optional mode: batch or streaming".into()),
    };
    let mut assets = fulgur_core::AssetBundle::default();
    assets.add_font_file(font)?;
    let options = fulgur_raikiri::RenderOptions {
        assets: Some(&assets),
        system_fonts: false,
        ..Default::default()
    };
    let pdf = render(Path::new(input), &Default::default(), &options)?;
    std::fs::write(output, pdf)?;
    Ok(())
}
