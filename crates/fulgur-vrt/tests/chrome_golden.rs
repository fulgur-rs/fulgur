//! Regenerate the Chromium goldens under `goldens/chrome/`. Ignored by
//! default: it needs the pinned Chrome build (see `chrome::CHROME_VERSION`)
//! and is run by the `VRT Chrome Golden Update` workflow.
#![cfg(feature = "chrome-golden")]

use fulgur_vrt::chrome;
use fulgur_vrt::runner::RunnerContext;

#[test]
#[ignore = "needs the pinned Chrome build; run by vrt-chrome-update.yml"]
fn generate_chrome_goldens() {
    let ctx = RunnerContext::discover().expect("discover context");
    let written = chrome::generate_goldens(&ctx).expect("generate Chromium goldens");
    eprintln!("wrote {} Chromium goldens", written.len());
}
