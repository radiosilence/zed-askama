//! The tree-sitter grammar for Askama templates, with the default delimiters.
//!
//! ```
//! let mut parser = tree_sitter::Parser::new();
//! parser
//!     .set_language(&tree_sitter_askama::LANGUAGE.into())
//!     .expect("the grammar matches the tree-sitter version");
//! let tree = parser.parse("{% if user.admin %}{{ user.name }}{% endif %}", None);
//! assert!(!tree.expect("no timeout was set").root_node().has_error());
//! ```

use tree_sitter_language::LanguageFn;

#[expect(unsafe_code, reason = "the generated parser is C, reached through FFI")]
// SAFETY: `tree_sitter_askama` is defined by `src/parser.c`, which build.rs compiles into
// this crate, with this signature.
unsafe extern "C" {
    fn tree_sitter_askama() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for Askama templates.
#[expect(unsafe_code, reason = "the generated parser is C, reached through FFI")]
// SAFETY: `tree_sitter_askama` is the generated parser's constructor, which
// returns a pointer to a static `TSLanguage`.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_askama) };

/// The grammar's `node-types.json`, describing every node and field.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
