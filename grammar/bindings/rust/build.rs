//! Compiles the generated parser and the external scanner into the crate.

fn main() {
    let src = std::path::Path::new("src");
    let mut build = cc::Build::new();
    build.std("c11").include(src).warnings(false);

    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");

    for file in ["parser.c", "scanner.c"] {
        let path = src.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        build.file(path);
    }
    build.compile("tree-sitter-askama");
}
