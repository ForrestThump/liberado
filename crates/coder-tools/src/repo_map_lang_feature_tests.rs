//! Language-feature gates for `repo_map` detection/extraction.
//!
//! Split from `repo_map.rs` / `repo_map_survivor_tests.rs` so feature-on and
//! feature-off coverage does not inflate those modules' health metrics.

use super::*;
use std::collections::HashSet;
use tree_sitter::Language;

#[cfg(feature = "lang-typescript")]
#[test]
fn detect_lang_covers_ts_variants() {
    assert!(detect_lang("src/app.ts").is_some(), ".ts detected");
    assert!(detect_lang("src/app.js").is_some(), ".js detected");
    assert!(detect_lang("src/app.jsx").is_some(), ".jsx detected");
}


/// The TypeScript and Go query sources stay loadable and productive.
#[cfg(all(feature = "lang-typescript", feature = "lang-go"))]
#[test]
fn extract_tags_supports_typescript_and_go() {
    let ts_source = "function hello(): void {\n  world();\n}\nfunction world(): void {}\n";
    let (ts_name, ts_lang) = detect_lang("a.ts").unwrap();
    let ts_tags = extract_tags("a.ts", ts_source, ts_name, &ts_lang);
    let ts_defs: Vec<&str> = ts_tags
        .iter()
        .filter(|t| t.is_def)
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        ts_defs.contains(&"hello") && ts_defs.contains(&"world"),
        "typescript definitions found: {ts_defs:?} ({ts_name})"
    );

    let go_source = "package main\n\nfunc main() {\n\thelper()\n}\n\nfunc helper() {}\n";
    let (go_name, go_lang) = detect_lang("m.go").unwrap();
    let go_tags = extract_tags("m.go", go_source, go_name, &go_lang);
    let go_defs: Vec<&str> = go_tags
        .iter()
        .filter(|t| t.is_def)
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        go_defs.contains(&"main") && go_defs.contains(&"helper"),
        "go definitions found: {go_defs:?} ({go_name})"
    );
    assert!(
        go_tags.iter().any(|t| !t.is_def && t.name == "helper"),
        "helper call captured as reference: {:?}",
        go_tags
    );
}


#[cfg(not(feature = "lang-typescript"))]
#[test]
fn detect_lang_ts_variants_unavailable_without_feature() {
    assert!(
        detect_lang("src/app.ts").is_none(),
        ".ts must be None without lang-typescript"
    );
    assert!(
        detect_lang("src/app.js").is_none(),
        ".js must be None without lang-typescript"
    );
    assert!(
        detect_lang("src/app.jsx").is_none(),
        ".jsx must be None without lang-typescript"
    );
}


#[cfg(not(feature = "lang-go"))]
#[test]
fn detect_lang_go_unavailable_without_feature() {
    assert!(
        detect_lang("m.go").is_none(),
        ".go must be None without lang-go"
    );
}


#[cfg(feature = "lang-python")]
#[test]
fn test_detect_lang_python() {
    let (name, _lang) = detect_lang("app/views.py").unwrap();
    assert_eq!(name, "python");
}


#[cfg(feature = "lang-typescript")]
#[test]
fn test_detect_lang_typescript() {
    let (name, _lang) = detect_lang("src/App.tsx").unwrap();
    assert_eq!(name, "tsx");
}


#[cfg(feature = "lang-go")]
#[test]
fn test_detect_lang_go() {
    let (name, _lang) = detect_lang("pkg/handler.go").unwrap();
    assert_eq!(name, "go");
}


#[cfg(not(feature = "lang-python"))]
#[test]
fn test_detect_lang_python_unavailable_without_feature() {
    assert!(detect_lang("app/views.py").is_none());
}


#[cfg(not(feature = "lang-typescript"))]
#[test]
fn test_detect_lang_typescript_unavailable_without_feature() {
    assert!(detect_lang("src/App.tsx").is_none());
    assert!(detect_lang("src/app.ts").is_none());
}


#[cfg(not(feature = "lang-go"))]
#[test]
fn test_detect_lang_go_unavailable_without_feature() {
    assert!(detect_lang("pkg/handler.go").is_none());
}


#[cfg(feature = "lang-python")]
#[test]
fn test_extract_python_tags() {
    let source = r#"
class App:
def run(self):
    self.init()

def init(self):
    pass

def main():
app = App()
app.run()
"#;
    let lang: Language = tree_sitter_python::LANGUAGE.into();
    let tags = extract_tags("test.py", source, "python", &lang);

    let def_names: HashSet<_> = tags
        .iter()
        .filter(|t| t.is_def)
        .map(|t| t.name.as_str())
        .collect();
    assert!(def_names.contains("App"));
    assert!(def_names.contains("run"));
    assert!(def_names.contains("init"));
    assert!(def_names.contains("main"));
}


