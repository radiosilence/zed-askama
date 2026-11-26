//! The text rust-analyzer sees for a Rust file that defines template items.
//!
//! It is the file as the user has it, with each item's `Template` derive and `#[template]`
//! attributes blanked with spaces of the same length, so rust-analyzer does not expand
//! askama's derive itself, and askama's generated code for the item inserted right after the
//! item, in its own scope, where its imports and private fields are visible. The generator's
//! `compile_error!` statements are blanked as well, since the generator reports those errors.
//!
//! Blanking keeps every byte where it was, so an offset in the user's text maps into the
//! overlay by adding the lengths of the insertions before it.

use std::ops::Range;

use askama_analyzer_codegen::Generated;

use crate::index::TemplateItem;

/// A Rust file as rust-analyzer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    text: String,
    /// In order of position.
    insertions: Vec<Insertion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Insertion {
    /// Where the code goes in the user's text: the end of the item.
    at: usize,
    /// Where the generated code sits in the overlay.
    code: Range<usize>,
    /// The item's index in the list the overlay was built from.
    item: usize,
}

/// Where an offset of the overlay comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The user's text, at this offset.
    Original(usize),
    /// An item's generated code, at this offset of [`Generated::code`].
    Generated { item: usize, offset: usize },
}

/// Written before and after each insertion, so generated code never joins a line of the user's.
const SEPARATOR: &str = "\n";

impl Overlay {
    /// The overlay of `original`, whose template items are `items`, each with what askama
    /// generated for it. An item with no generated code is left out; its derive is still
    /// blanked, as rust-analyzer could only report askama's errors a second time.
    pub fn build(original: &str, items: &[(&TemplateItem, Option<&Generated>)]) -> Self {
        let mut blanked = original.to_owned();
        for (item, _) in items {
            for range in &item.blank {
                blank(&mut blanked, range);
            }
        }
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by_key(|&at| items[at].0.range.end);

        let mut text = String::with_capacity(original.len());
        let mut insertions = Vec::new();
        let mut copied = 0;
        for index in order {
            let (item, generated) = items[index];
            let Some(generated) = generated else {
                continue;
            };
            let at = item.range.end.min(blanked.len());
            text.push_str(&blanked[copied..at]);
            text.push_str(SEPARATOR);
            let start = text.len();
            let mut code = generated.code.clone();
            for error in &generated.errors {
                blank(&mut code, &error.generated);
            }
            text.push_str(&code);
            insertions.push(Insertion {
                at,
                code: start..text.len(),
                item: index,
            });
            text.push_str(SEPARATOR);
            copied = at;
        }
        text.push_str(&blanked[copied..]);
        Self { text, insertions }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The overlay offset of an offset in the user's text. An offset at an item's end is
    /// taken to be the next character, after the code inserted there.
    pub fn from_original(&self, offset: usize) -> usize {
        offset
            + self
                .insertions
                .iter()
                .take_while(|insertion| insertion.at <= offset)
                .map(|insertion| insertion.code.len() + 2 * SEPARATOR.len())
                .sum::<usize>()
    }

    /// The overlay offset of an offset in an item's generated code.
    pub fn from_generated(&self, item: usize, offset: usize) -> Option<usize> {
        let insertion = self
            .insertions
            .iter()
            .find(|insertion| insertion.item == item)?;
        let at = insertion.code.start + offset;
        (at <= insertion.code.end).then_some(at)
    }

    /// Where an overlay offset comes from. Offsets in the separators belong to the user's
    /// text, at the item's end.
    pub fn place(&self, offset: usize) -> Place {
        let mut shift = 0;
        for insertion in &self.insertions {
            let block_start = insertion.code.start - SEPARATOR.len();
            if offset < block_start {
                break;
            }
            if insertion.code.contains(&offset)
                || offset == insertion.code.end && offset > insertion.code.start
            {
                return Place::Generated {
                    item: insertion.item,
                    offset: offset - insertion.code.start,
                };
            }
            let block_end = insertion.code.end + SEPARATOR.len();
            if offset < block_end {
                return Place::Original(insertion.at);
            }
            shift += block_end - block_start;
        }
        Place::Original(offset - shift)
    }
}

/// Replaces a range with spaces of the same length in bytes. Ranges come from spans, so
/// they fall on character boundaries; one that does not is left alone.
fn blank(text: &mut String, range: &Range<usize>) {
    if range.end <= text.len()
        && text.is_char_boundary(range.start)
        && text.is_char_boundary(range.end)
    {
        text.replace_range(range.clone(), &" ".repeat(range.len()));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::rc::Rc;

    use askama_analyzer_codegen::{Files, Generated, Input};

    use super::{Overlay, Place};
    use crate::index::template_items;

    struct NoFiles;

    impl Files for NoFiles {
        fn read(&self, _: &Path) -> std::io::Result<String> {
            Err(std::io::ErrorKind::NotFound.into())
        }
    }

    fn generate(item: &str) -> Generated {
        askama_analyzer_codegen::generate(
            Input {
                item,
                file: Path::new("/crate/src/lib.rs"),
                manifest_dir: Path::new("/crate"),
            },
            Rc::new(NoFiles),
        )
        .expect("the item generates")
    }

    const FILE: &str = "use askama::Template;\n#[derive(Debug, Template)]\n#[template(source = \"{{ name }}\", ext = \"txt\")]\nstruct A { name: String }\nfn after() {}\n";

    fn overlay() -> (Overlay, Generated) {
        let items = template_items(FILE).expect("fixture parses");
        let generated = generate(&FILE[items[0].range.clone()]);
        let overlay = Overlay::build(FILE, &[(&items[0], Some(&generated))]);
        (overlay, generated)
    }

    #[test]
    fn the_derive_and_template_attribute_are_blanked_and_the_code_inserted_after_the_item() {
        let (overlay, generated) = overlay();
        let text = overlay.text();
        assert!(
            text.contains("#[derive(Debug          )]"),
            "Template is blanked: {text}"
        );
        assert!(
            !text.contains("#[template("),
            "the attribute is blanked: {text}"
        );
        let item_end = text
            .find("struct A { name: String }")
            .expect("the item is kept")
            + "struct A { name: String }".len();
        assert!(
            text[item_end..]
                .trim_start()
                .starts_with(generated.code.trim_start()),
            "the generated code follows the item"
        );
        assert!(
            text.trim_end().ends_with("fn after() {}"),
            "the rest of the file follows"
        );
    }

    #[test]
    fn offsets_before_the_insertion_are_unchanged_and_offsets_after_shift() {
        let (overlay, _) = overlay();
        let before = FILE.find("struct A").expect("fixture holds the item");
        assert_eq!(
            overlay.from_original(before),
            before,
            "nothing was inserted before the item"
        );
        let after = FILE.find("fn after").expect("fixture holds the function");
        let shifted = overlay.from_original(after);
        assert_eq!(
            &overlay.text()[shifted..shifted + 8],
            "fn after",
            "an offset after the item lands on the same text"
        );
        assert_eq!(
            overlay.place(shifted),
            Place::Original(after),
            "and maps back"
        );
    }

    #[test]
    fn an_offset_in_the_generated_code_is_placed_in_it_and_maps_back() {
        let (overlay, generated) = overlay();
        let mapping = generated.map.first().expect("something is mapped");
        let offset = overlay
            .from_generated(0, mapping.generated.start)
            .expect("the item has code in the overlay");
        assert_eq!(
            overlay.place(offset),
            Place::Generated {
                item: 0,
                offset: mapping.generated.start
            },
            "a generated offset round-trips"
        );
        assert_eq!(
            &overlay.text()[offset..offset + mapping.generated.len()],
            &generated.code[mapping.generated.clone()],
            "the generated text is where the map says"
        );
    }

    #[test]
    fn the_overlay_is_valid_rust() {
        let (overlay, _) = overlay();
        syn::parse_file(overlay.text())
            .unwrap_or_else(|err| panic!("not valid Rust: {err}\n{}", overlay.text()));
    }
}
