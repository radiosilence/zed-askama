# zed-askama

[Askama](https://askama.rs) support for [Zed](https://zed.dev): highlighting and structure from a tree-sitter grammar, and completion, hover, go to definition, references, renames and errors inside templates from the rust-analyzer you already run.

It is being built; [issue #1](https://github.com/radiosilence/zed-askama/issues/1) has the plan.

## Why

Askama compiles a template into Rust inside `#[derive(Template)]`. rust-analyzer analyses that expansion but cannot map it back to the template, so a broken template shows up as one error on the struct, and the template itself gets no completion or navigation. Existing editor support either highlights only, or approximates askama's code generation by hand and misses whatever the approximation does not cover.

`askama-analyzer` runs askama's own code generator, keeps the template position of every token it generates, and asks rust-analyzer about the result. Answers come back in template positions, and they are the compiler's answers, because the code is the compiler's code.

## How it is put together

| Path | What |
|---|---|
| `crates/askama-analyzer-codegen` | askama's generator run on one template item, with a map from every generated token to its template range |
| `vendor/askama_derive` | askama's generator with a small patch for long-running use, described in [ARCHITECTURE.md](ARCHITECTURE.md) |

## Licence

MIT, in [LICENSE](LICENSE). `vendor/askama_derive` is askama's code under its own MIT or Apache-2.0 licence, kept beside it.
