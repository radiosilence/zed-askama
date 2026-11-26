# Changelog

## 0.1.1

### Added

- **Completion offers loop variables, tags, filters and template paths, and go to definition follows includes, macros and blocks.** Completion of a bare name offered only the template struct's fields; it now offers the loop variables and `let` bindings in scope too, tag keywords with the closing tag of the innermost open block first, askama's built-in filters after a pipe, and template files inside `extends`, `include` and `import`. Go to definition takes a path to its file, a `call` to its macro and an overriding block to the parent's. Zed shows rust-analyzer loading as progress, and a Claude Code plugin gives Claude Code's LSP tool the same analysis.

### Changed

- **The extension downloads askama-analyzer from its own repository.** The language server is now developed and released in radiosilence/askama-analyzer, so it ships on its own schedule and installed extensions take its newest release; this repository keeps the grammar and the extension.

## 0.1.0

### Added

- **Askama templates get completion, navigation and errors in Zed.** Askama turns a template into Rust inside a derive macro, so rust-analyzer could only report a broken template as an error on the struct. The new Zed extension highlights templates with a grammar of its own and runs `askama-analyzer`, which feeds askama's generated code to the user's own rust-analyzer, as Zed's `lsp.rust-analyzer` settings configure it, and maps completion, hover, definitions, references, renames and errors back to the template. The extension downloads askama-analyzer from this repository's releases and keeps it current, unless `lsp.askama-analyzer.binary` names one.
- **Templates can be formatted.** `askama-analyzer fmt` (and formatting in the editor) gives tags one space inside their delimiters and Rust-style spacing in expressions, filters, patterns and arguments, leaving text outside tags, raw blocks, comments' text and line breaks inside tags as written; it refuses any change that would alter what askama parses, and `--check` lists unformatted templates for CI.
