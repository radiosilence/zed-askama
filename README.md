# zed-askama

[Askama](https://askama.rs) support for [Zed](https://zed.dev): highlighting and structure from a tree-sitter grammar, and completion, hover, go to definition, references, renames, errors and formatting inside templates from [askama-analyzer](https://github.com/radiosilence/askama-analyzer), which asks the rust-analyzer you already run.

## Why

Askama compiles a template into Rust inside `#[derive(Template)]`. rust-analyzer analyses that expansion but cannot map it back to the template, so a broken template shows up as one error on the struct, and the template itself gets no completion or navigation. askama-analyzer runs askama's own code generator, keeps the template position of every token it generates, and asks rust-analyzer about the result, so its answers are the compiler's. This repository is what Zed needs around it: the grammar, and the extension that downloads the server and hands it your rust-analyzer settings.

### Ecosystem

Some projects I've made for the askama/axum/datastar stack:

- [askama-analyzer](https://github.com/radiosilence/askama-analyzer): Language server for askama templates.
- [askama-datastar](https://github.com/radiosilence/askama-datastar): Helpers for using Askama with datastar nicely.
- [askama-gettext](https://github.com/radiosilence/askama-gettext): Helpers for doing i18 nicely using standardised gettext (po, pot, etc) files.
- [zed-askama](https://github.com/radiosilence/zed-askama): Zed extension to support the LSP with all the bells and whistles.
- [koan-kit-web](https://github.com/radiosilence/koan/tree/main/koan-kit-web): An example component library based on Askama and datastar.

## Trying it in Zed

Until it is in Zed's extension registry, install it as a dev extension: in Zed, open the command palette, run **zed: install dev extension**, and choose this repository's directory.

Files ending `.askama` or `.rinja` open as Askama HTML, `.md.askama` as Askama Markdown and `.txt.askama` as Askama Text.

Templates named `*.html`, as most are, cannot be told apart from plain HTML by the extension itself: Zed lets an extension's language claim files by suffix only, and claiming `.html` would take every HTML file away from Zed's HTML support. Name where your templates live with a `file_types` setting. In the project's `.zed/settings.json`, committed, it applies to everyone who opens the repository:

```json
{
  "file_types": {
    "Askama HTML": ["**/templates/**/*.html"]
  }
}
```

Without it, a template still gets completion, hover, definitions, references, renames, errors and formatting, since the language server also attaches to HTML files; only the template-aware highlighting, brackets and outline need the file to open as Askama HTML.

The extension downloads the newest askama-analyzer release and keeps it up to date. To run your own build instead, set `lsp.askama-analyzer.binary.path` in Zed's settings.

## How it is put together

| Path                                                | What                                                                                                                                                                           |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `grammar/`                                          | The tree-sitter grammar, its corpus tests, and a differential test holding it to askama's own parser                                                                           |
| `extension.toml`, `languages/`, `snippets/`, `src/` | The Zed extension: three languages (HTML, Markdown and plain text content), their queries and snippets, and the Rust that finds or downloads askama-analyzer and rust-analyzer |
| `vendor/askama-testing`                             | askama's own test suite at the pinned release, which the grammar is tested against                                                                                             |
| `docs/releasing.md`                                 | How a release goes, the secrets it needs, and the first submission to Zed's registry                                                                                           |

## Licence

MIT, in [LICENSE](LICENSE). `vendor/askama-testing` is askama's code under its own MIT or Apache-2.0 licence, kept beside it.
