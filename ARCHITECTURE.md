# Architecture

How `askama-analyzer` answers questions about templates, and the rules that keep its answers the compiler's. Issue #1 holds the plan this follows.

## The generator is askama's own

rust-analyzer analyses Rust in a crate's module tree, and askama turns a template into Rust inside `#[derive(Template)]`, an expansion rust-analyzer cannot map back to the template. The analyser therefore produces that Rust itself, with askama's own generator, and hands it to rust-analyzer as part of an in-memory copy of the struct's file.

- **The generator is askama's, never an imitation.** *Why:* the code rust-analyzer analyses is then the code `cargo build` compiles, so the editor and the compiler cannot disagree about a template, and a new askama feature needs no work here beyond re-vendoring.
- **Every generated token carries the template range it came from.** askama's generator already attaches a span to nearly every token it emits (`span_for_node`), and proc-macro2's fallback implementation, outside a compiler and with `span-locations`, gives each span an exact byte range. `askama_analyzer_codegen::generate` prints the tokens as Rust and records a `Mapping` from each printed token to its `Origin`: a template file and a byte range, or a range of the item's own text (its names and types, and an inline `source` literal). *Why:* positions then map in both directions without guessing, for completion, hover, definitions, diagnostics and renames alike.
- **Files come from the caller.** The generator reads templates and `askama.toml` through `Files`, never the disk. *Why:* an editor's unsaved buffers are the truth, and a crate that does no I/O is tested with no project on disk.

## The tooling patch

askama's generator is written for a compiler, where a process expands a crate once and exits. `vendor/askama_derive-tooling.patch` adds an opt-in `tooling` feature for a long-running caller; `scripts/vendor-askama.py` applies it to the release from crates.io, verified against the index's checksum. With a `tooling::Host` set (by `askama_derive::tooling::expand`):

| Change | Why |
|---|---|
| Template ranges are kept outside a proc macro (`span_for_node`, compile errors) | upstream drops them unless `proc_macro::is_available()`, since spans mean nothing to a compiler that is not running |
| Each template file read gets a located literal of its own, recorded in `Expansion::sources` | upstream gives every file the derive's literal, so tokens from a parent, include or import could only point into the wrong text |
| Templates and `askama.toml` are read through the host, and a template path resolves by asking the host whether the file exists | an editor's unsaved buffers must win over the disk, including a new template not yet saved |
| An error whose span is not already in a template is placed where its file info points | upstream turns that location into text appended to the message, which an editor cannot place |
| A macro argument such as `a.name` is printed compactly before askama splits it on `.` | askama reads that argument back from printed tokens, which a compiler prints as `a.name` and proc-macro2 outside one as `a . name`; the split then made identifiers with spaces in them, and the generator panicked on koan's pages |
| A block's and a macro body's buffered expressions are written with the block's or macro's own context, before control returns to the caller | askama buffers `{{ }}` expressions and writes them with whatever context is current at the next flush, so a child's block or an imported macro would be located in the caller's file. Writes can therefore be grouped differently from a compiler's output; each expression is generated identically, so what rust-analyzer checks does not change |
| The process-wide template cache and parsed-template cache are bypassed | the first is keyed by path and never invalidated, so edits would be ignored; the second is keyed by content, so it would grow with every edit |
| The manifest directory and the calling file come from the host | upstream reads `CARGO_MANIFEST_DIR` and the call-site span, which a server serving many crates does not have |

Without a host the patched generator behaves exactly as upstream's. The same change is proposed upstream; once it lands the vendoring goes. Until then the crates are not published to crates.io, which refuses a path dependency.

## The server

`askama-analyzer` serves the protocol on stdin and stdout. Its parts, and the rules each keeps:

- **One ordered lane owns the analysis** (`server.rs`). Notifications only record a document's whole text in a table shared with the lane and wake it; the lane waits for edits to settle (150 ms, at most a second), applies every change in order, sends rust-analyzer what changed and publishes the diagnostics that changed. Requests go down the same lane, after any change that arrived before them. *Why:* a change is a snapshot of a document, never a delta, so a lane that falls behind loses intermediate states and never a change; and a request sees the documents as the user sees them.
- **One thread runs askama's generator and `syn`** (`generator.rs`). *Why:* both keep thread-local state (the tooling host, proc-macro2's source map), freed after each job; one named thread keeps it in one place and the async workers free of it.
- **The index finds items with `syn`** (`index.rs`): every struct and enum deriving `Template`, nested ones included, with the byte ranges of the derive's `Template` path (and one comma beside it) and each `#[template]` attribute. A file that stops parsing keeps its last index, since it is being typed.
- **A template's owners are the items whose generation read it** (`Generated::templates`), so `extends`, `include` and `import` are followed by askama itself, never by a second resolver.

## The overlay and rust-analyzer

Zed gives an extension no way to add documents to the rust-analyzer it runs for Rust, so the server runs a private one per workspace (`ra.rs`), with the binary and settings the Zed extension resolves from the user's own `lsp.rust-analyzer` settings. Its own settings override four things: no check on save (the user's instance runs it), its own target directory (so it never waits on the user's cargo lock), no auto-import or postfix completions (they would edit Rust code), and its own file watching. It starts when a template of its workspace is open and stops after ten idle minutes, since it holds the whole workspace's analysis in memory.

- **Each Rust file defining template items reaches it as an overlay** (`overlay.rs`): the user's text, with each item's `Template` derive and `#[template]` attributes blanked with spaces of the same length and the generated code inserted right after the item, in its own scope, so its imports and private fields are visible. The generator's `compile_error!` statements are blanked too. *Why:* blanking keeps every byte where it was, so an offset in the user's text maps into the overlay by adding the lengths of the insertions before it, and rust-analyzer never expands askama's derive a second time.
- **A question maps in three steps** (`workspace.rs`): the generator's map gives the generated token a template offset came from (the most specific one covering it), the overlay gives that token's place, and rust-analyzer is asked there. Answers come back the same way; an answer in the user's own Rust code passes through. The client and rust-analyzer are given one position encoding, so positions in files the server does not rewrite need no conversion.
- **Completion asks about a probe** (`features.rs`): the item generated with a marker identifier typed at the cursor, so a half-typed `{{ user. }}` parses, with the marker removed from the code before rust-analyzer sees it. The completions' edits around the cursor in the probe are the same text around the cursor in the template. The clean overlay is put back after.
- **A rename edit lands only where the text it replaces is the old name.** *Why:* generated code askama wrote itself, with no template text behind it, can map to a nearby template range and must not be allowed to rewrite it.
- **rust-analyzer's errors in generated code are published at their template positions**, from the overlay version they were computed for and no other.

## Memory

proc-macro2's fallback keeps every text it has parsed, per thread, for the life of the thread when `span-locations` is on. `generate` reads every span into plain ranges and then calls `proc_macro2::extra::invalidate_current_thread_spans`, so a thread can generate indefinitely without growing. Any other code that parses with proc-macro2 (`syn` over Rust files) on a long-lived thread must do the same.
