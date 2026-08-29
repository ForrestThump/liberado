//! Split from `read_search_tests.rs` with the symbol-extraction family.

use super::*;

#[test]
fn extract_rust_visibility_variants() {
    let content = "pub(crate) fn internal() {}\npub(super) fn semi_public() {}\npub fn public() {}\nfn private() {}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(symbols.contains(&"fn internal".to_string()));
    assert!(symbols.contains(&"fn semi_public".to_string()));
    assert!(symbols.contains(&"fn public".to_string()));
    assert!(symbols.contains(&"fn private".to_string()));
}

#[test]
fn extract_rust_const_and_unsafe_fn() {
    let content = "const fn compile_time() -> u32 { 42 }\npub const fn pub_compile() {}\nunsafe fn raw_op() {}\npub unsafe fn pub_raw() {}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(symbols.contains(&"fn compile_time".to_string()));
    assert!(symbols.contains(&"fn pub_compile".to_string()));
    assert!(symbols.contains(&"fn raw_op".to_string()));
    assert!(symbols.contains(&"fn pub_raw".to_string()));
}

#[test]
fn extract_rust_attribute_prefixed() {
    let content = "#[derive(Debug)]\npub struct Foo;\n\n#[inline]\npub fn bar() {}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(symbols.contains(&"struct Foo".to_string()));
    assert!(symbols.contains(&"fn bar".to_string()));
}

#[test]
fn extract_rust_generic_impl() {
    let content = "impl<T> MyType<T> {\n    fn new() -> Self {}\n}\n\nimpl PlainType {\n    fn method() {}\n}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(
        symbols.iter().any(|s| s.contains("MyType")),
        "should find generic impl"
    );
    assert!(symbols.contains(&"impl PlainType".to_string()));
}

#[test]
fn extract_ts_export_default_function() {
    let content = "export default function main() {}\nexport function helper() {}";
    let symbols = extract_symbols("app.ts", content);
    assert!(symbols.contains(&"export function main".to_string()));
    assert!(symbols.contains(&"export function helper".to_string()));
}

#[test]
fn extract_rust_traits_and_enums() {
    let content =
        "pub trait Handler {\n    fn handle(&self);\n}\n\npub enum Status {\n    Ok,\n    Err,\n}";
    let symbols = extract_symbols("src/types.rs", content);
    assert!(symbols.contains(&"trait Handler".to_string()));
    assert!(symbols.contains(&"enum Status".to_string()));
}

#[test]
fn extract_rust_mods_and_impls() {
    let content = "pub mod sub;\nmod private_mod {\n}\n\nimpl MyType {\n    fn new() -> Self {}\n}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(symbols.contains(&"mod sub".to_string()));
    assert!(symbols.contains(&"mod private_mod".to_string()));
    assert!(symbols.contains(&"impl MyType".to_string()));
}

#[test]
fn extract_python_symbols() {
    let content =
        "def handle_request(req):\n    pass\n\nclass Router:\n    def route(self):\n        pass";
    let symbols = extract_symbols("app.py", content);
    assert!(symbols.contains(&"def handle_request".to_string()));
    assert!(symbols.contains(&"class Router".to_string()));
    // Indentation is Python's nesting. `route` is a method on `Router`; listing it beside the
    // module-level names says the module exports something it does not. The guard for this
    // was already written — it just never saw an indented line to reject.
    assert!(
        !symbols.contains(&"def route".to_string()),
        "a nested def is not a top-level symbol: {symbols:?}"
    );
}

#[test]
fn extract_typescript_symbols() {
    let content = "export function main() {}\n\nclass App {\n    render() {}\n}\n\ninterface Config {\n    port: number;\n}\n\nconst handler = (req: Request) => { return 200; };";
    let symbols = extract_symbols("app.ts", content);
    assert!(symbols.contains(&"export function main".to_string()));
    assert!(symbols.contains(&"class App".to_string()));
    assert!(symbols.contains(&"interface Config".to_string()));
    assert!(symbols.contains(&"const handler".to_string()));
}

#[test]
fn extract_go_symbols() {
    let content = "func main() {\n}\n\ntype Server struct {\n    port int\n}\n\ntype Handler interface {\n    Serve()\n}";
    let symbols = extract_symbols("main.go", content);
    assert!(symbols.contains(&"func main".to_string()));
    assert!(symbols.contains(&"type Server struct".to_string()));
    assert!(symbols.contains(&"type Handler interface".to_string()));
}

#[test]
fn extract_java_symbols() {
    let content = "public class Main {\n    public static void main(String[] args) {}\n}\n\npublic enum State {\n    ON, OFF\n}\n\npublic interface Runnable {\n    void run();\n}";
    let symbols = extract_symbols("Main.java", content);
    assert!(symbols.contains(&"class Main".to_string()));
    assert!(symbols.contains(&"enum State".to_string()));
    assert!(symbols.contains(&"interface Runnable".to_string()));
}

#[test]
fn unknown_extension_returns_empty() {
    let content = "fn main() {}";
    let symbols = extract_symbols("README.md", content);
    assert!(symbols.is_empty());
}

#[test]
fn skips_comments() {
    let content = "// fn not_a_fn() {}\n/* struct Fake {} */\n\nfn real() {}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert_eq!(symbols.len(), 1);
    assert!(symbols.contains(&"fn real".to_string()));
}

#[test]
fn extract_java_non_public_class_interface_enum() {
    assert_eq!(
        extract_java_symbol("class PackagePrivate { }"),
        Some("class PackagePrivate".to_string())
    );
    assert_eq!(
        extract_java_symbol("interface Contract { }"),
        Some("interface Contract".to_string())
    );
    assert_eq!(
        extract_java_symbol("enum Color { RED, GREEN }"),
        Some("enum Color".to_string())
    );
}

#[test]
fn extract_ts_plain_and_exported_functions() {
    assert_eq!(
        extract_ts_symbol("function foo(a) {"),
        Some("function foo".into())
    );
    assert_eq!(
        extract_ts_symbol("export function bar(a) {"),
        Some("export function bar".into())
    );
    assert_eq!(
        extract_ts_symbol("export default async function baz(a) {"),
        Some("export function baz".into()),
    );
}

#[test]
fn extract_ts_classes_interfaces_and_consts() {
    assert_eq!(extract_ts_symbol("class Foo {"), Some("class Foo".into()));
    assert_eq!(
        extract_ts_symbol("export abstract class Bar implements X {"),
        Some("class Bar".into()),
    );
    assert_eq!(
        extract_ts_symbol("interface IFoo {"),
        Some("interface IFoo".into())
    );
    assert_eq!(
        extract_ts_symbol("export interface IFoo {"),
        Some("interface IFoo".into()),
    );
    assert_eq!(
        extract_ts_symbol("const pick = (a) => a[0]"),
        Some("const pick".into()),
    );
}

#[test]
fn extract_ts_ignores_comments_and_non_declarations() {
    assert_eq!(extract_ts_symbol("// comment"), None);
    assert_eq!(extract_ts_symbol("const plain = 42"), None);
    assert_eq!(extract_ts_symbol("not a symbol"), None);
}
