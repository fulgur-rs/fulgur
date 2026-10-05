# fulgur-dev

Unpublished development CLI for selecting Fulgur's layout backend.

```sh
cargo run -p fulgur-dev -- render input.html --engine blitz -o output.pdf
cargo run -p fulgur-dev -- render input.html --engine raikiri -o output.pdf
```

The default engine is `blitz`. Both the input file and `-o` output file are
required. The CLI selects an independent backend entry point with the contract
**input file path → PDF bytes**, then writes the bytes to the output file only
after the backend succeeds.

Blitz uses the existing Fulgur PDF renderer. Relative resources are resolved
against the input file's parent directory.

Raikiri lays out the document with Raikiri and draws the PDF with Fulgur's
Krilla painter for that backend, using Fulgur's default page size and margins.
Relative resources are resolved against the input file's parent directory, and
files outside it are not read. The output file is written only on success.

This CLI is separate from the published `fulgur` CLI and bindings. It does not
add a shared backend trait or change the published facade.
