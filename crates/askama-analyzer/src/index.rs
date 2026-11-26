//! The items in a Rust file that derive `Template`, found with `syn`.
//!
//! [`template_items`] parses with proc-macro2, so it runs on the generator thread, which frees
//! proc-macro2's spans after each job.

use std::borrow::Cow;
use std::ops::Range;

use proc_macro2::Span;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Ident, Token};

/// An item deriving `Template`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateItem {
    /// The item's name, for messages.
    pub name: String,
    /// The item, its attributes included, as a byte range of the file.
    pub range: Range<usize>,
    /// What the overlay blanks so rust-analyzer does not expand askama's derive a second time:
    /// the `Template` path in `#[derive(...)]` with one comma beside it, and each
    /// `#[template(...)]` attribute. Byte ranges of the file.
    pub blank: Vec<Range<usize>>,
}

/// Every item deriving `Template` in a Rust file, nested ones included (askama's own tests
/// define template structs inside functions). A file that does not parse is an error, so the
/// caller can keep what it found before.
pub fn template_items(text: &str) -> Result<Vec<TemplateItem>, syn::Error> {
    let file = syn::parse_str::<syn::File>(&without_shebang(text))?;
    let mut finder = Finder::default();
    finder.visit_file(&file);
    Ok(finder.items)
}

/// A shebang line is not Rust tokens; spaces of the same length keep every offset.
fn without_shebang(text: &str) -> Cow<'_, str> {
    if text.starts_with("#!") && !text.starts_with("#![") {
        let end = text.find('\n').unwrap_or(text.len());
        Cow::Owned(" ".repeat(end) + &text[end..])
    } else {
        Cow::Borrowed(text)
    }
}

#[derive(Default)]
struct Finder {
    items: Vec<TemplateItem>,
}

impl<'ast> Visit<'ast> for Finder {
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.consider(&item.attrs, &item.ident, item.span());
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.consider(&item.attrs, &item.ident, item.span());
        syn::visit::visit_item_enum(self, item);
    }
}

impl Finder {
    fn consider(&mut self, attrs: &[Attribute], ident: &Ident, span: Span) {
        let mut derives = false;
        let mut blank = Vec::new();
        for attr in attrs {
            if attr.path().is_ident("derive") {
                let Ok(paths) =
                    attr.parse_args_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)
                else {
                    continue;
                };
                let mut previous_comma: Option<Range<usize>> = None;
                for pair in paths.pairs() {
                    let path = pair.value();
                    let comma = pair.punct().map(|comma| comma.span().byte_range());
                    if path
                        .segments
                        .last()
                        .is_some_and(|segment| segment.ident == "Template")
                    {
                        derives = true;
                        let path_range = path.span().byte_range();
                        // The comma after it, or else the one before, so the list stays valid.
                        blank.push(match (&comma, &previous_comma) {
                            (Some(after), _) => path_range.start..after.end,
                            (None, Some(before)) => before.start..path_range.end,
                            (None, None) => path_range,
                        });
                    }
                    previous_comma = comma;
                }
            } else if attr.path().is_ident("template") {
                blank.push(attr.span().byte_range());
            }
        }
        if derives {
            self.items.push(TemplateItem {
                name: ident.to_string(),
                range: span.byte_range(),
                blank,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::template_items;

    fn blanked(text: &str) -> String {
        let items = template_items(text).expect("fixture parses");
        let mut out = text.to_owned();
        for range in items.iter().flat_map(|item| item.blank.iter()) {
            out.replace_range(range.clone(), &" ".repeat(range.len()));
        }
        out
    }

    #[test]
    fn an_item_is_found_with_its_attributes() {
        let text = "use askama::Template;\n\n/// Docs.\n#[derive(Template)]\n#[template(path = \"a.html\")]\nstruct A { x: u8 }\n";
        let items = template_items(text).expect("fixture parses");
        assert_eq!(items.len(), 1, "one item derives Template");
        let item = &items[0];
        assert_eq!(item.name, "A");
        assert_eq!(
            &text[item.range.clone()],
            "/// Docs.\n#[derive(Template)]\n#[template(path = \"a.html\")]\nstruct A { x: u8 }",
            "the range covers the doc comment, the attributes and the item"
        );
    }

    #[test]
    fn blanking_leaves_a_valid_derive_list_wherever_template_sits() {
        for (text, kept) in [
            (
                "#[derive(Template)] struct A;",
                "#[derive(        )] struct A;",
            ),
            (
                "#[derive(Debug, Template)] struct A;",
                "#[derive(Debug          )] struct A;",
            ),
            (
                "#[derive(Template, Debug)] struct A;",
                "#[derive(          Debug)] struct A;",
            ),
            (
                "#[derive(Clone, askama::Template, Debug)] struct A;",
                "#[derive(Clone,                   Debug)] struct A;",
            ),
        ] {
            let out = blanked(text);
            assert_eq!(out, kept, "blanking {text:?}");
            syn::parse_str::<syn::File>(&out)
                .unwrap_or_else(|err| panic!("{out:?} is not Rust: {err}"));
        }
    }

    #[test]
    fn the_template_attribute_is_blanked_whole() {
        let out = blanked(
            "#[derive(Template)]\n#[template(path = \"a.html\", ext = \"html\")]\nstruct A;",
        );
        assert!(!out.contains("template("), "the attribute is gone: {out:?}");
        assert_eq!(
            out.len(),
            "#[derive(Template)]\n#[template(path = \"a.html\", ext = \"html\")]\nstruct A;".len(),
            "offsets are kept"
        );
    }

    #[test]
    fn items_inside_functions_and_modules_are_found() {
        let text =
            "mod m { #[derive(Template)] struct A; }\nfn f() { #[derive(Template)] enum B { X } }";
        let names: Vec<_> = template_items(text)
            .expect("fixture parses")
            .into_iter()
            .map(|item| item.name)
            .collect();
        assert_eq!(names, ["A", "B"], "nested items are found");
    }

    #[test]
    fn a_shebang_line_does_not_stop_the_file_parsing() {
        let text = "#!/usr/bin/env rust-script\n#[derive(Template)] struct A;";
        let items = template_items(text).expect("a shebang is skipped");
        assert_eq!(
            &text[items[0].range.clone()],
            "#[derive(Template)] struct A;",
            "offsets count the shebang"
        );
    }
}
