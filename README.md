# zed-askama

[Askama](https://askama.rs) support for [Zed](https://zed.dev): highlighting and structure from a tree-sitter grammar, and completion, hover, go to definition, references, renames and errors inside templates from the rust-analyzer you already run.

It is being built; [issue #1](https://github.com/radiosilence/zed-askama/issues/1) has the plan.

## Why

Askama compiles a template into Rust inside `#[derive(Template)]`. rust-analyzer analyses that expansion but cannot map it back to the template, so a broken template shows up as one error on the struct, and the template itself gets no completion or navigation. Existing editor support either highlights only, or approximates askama's code generation by hand and misses whatever the approximation does not cover.

`askama-analyzer` runs askama's own code generator, keeps the template position of every token it generates, and asks rust-analyzer about the result. Answers come back in template positions, and they are the compiler's answers, because the code is the compiler's code.

## Trying it in Zed

Until it is in Zed's extension registry, install it as a dev extension: in Zed, open the command palette, run **zed: install dev extension**, and choose this repository's `editors/zed` directory.

Files ending `.askama` or `.rinja` open as Askama HTML, `.md.askama` as Askama Markdown and `.txt.askama` as Askama Text. Templates named `*.html`, as most are, need telling apart from plain HTML; add a `file_types` setting naming where they live, in your settings or the project's `.zed/settings.json`:

```json
{
  "file_types": {
    "Askama HTML": ["**/templates/**/*.html"]
  }
}
```

`askama-analyzer fmt [--check] [PATH...]` formats templates, the files given or every file under a `templates` directory, changing only the spacing inside tags: text outside them is what askama renders, so it is never touched.

## How it is put together

| Path | What |
|---|---|
| `grammar/` | The tree-sitter grammar, its corpus tests, and a differential test holding it to askama's own parser |
| `editors/zed/` | The Zed extension: three languages (HTML, Markdown and plain text content), their queries and snippets, and the Rust that finds or downloads the server and rust-analyzer |
| `docs/releasing.md` | How a release goes, the secrets it needs, and the first submission to Zed's registry |
| `crates/askama-analyzer-codegen` | askama's generator run on one template item, with a map from every generated token to its template range |
| `vendor/askama_derive` | askama's generator with a small patch for long-running use, described in [ARCHITECTURE.md](ARCHITECTURE.md) |
| `vendor/askama-testing` | askama's own test suite at the pinned release, which the grammar and the generator are tested against |

## Licence

MIT, in [LICENSE](LICENSE). `vendor/askama_derive` is askama's code under its own MIT or Apache-2.0 licence, kept beside it.
