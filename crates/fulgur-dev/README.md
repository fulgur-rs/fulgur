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

Raikiri currently reads the file and runs its native layout pipeline, then
returns `Raikiri PDF drawing is not implemented`. It does not create or
overwrite the output file on that failure. PDF drawing and resource bundle
integration will be added later; Raikiri input styles must currently be embedded
in the HTML.

This CLI is separate from the published `fulgur` CLI and bindings. It does not
add a shared backend trait or change the published facade.
