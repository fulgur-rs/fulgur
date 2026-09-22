use fulgur_raikiri_consumer_spike::{probe_layout, probe_streaming};
use serde_json::json;
use std::error::Error;

const SHORT_HTML: &str = "<!doctype html><html><body><p>hello</p></body></html>";

const FORCED_BREAK_HTML: &str = r#"<!doctype html><html><head><style>
    @page { margin: 0 }
    body { margin: 0 }
    .first { height: 20px; break-after: page }
    .second { height: 20px }
</style></head><body>
    <div class="first">first</div><div class="second">second</div>
</body></html>"#;

fn main() -> Result<(), Box<dyn Error>> {
    let report = json!({
        "layout": {
            "short": probe_layout(SHORT_HTML)?,
            "forced_break": probe_layout(FORCED_BREAK_HTML)?,
        },
        "streaming": probe_streaming(SHORT_HTML)?,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
