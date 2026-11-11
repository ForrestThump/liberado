//! Limits live in config, not in a baseline file.
//!
//! CRAP uses `threshold` from `.cargo-crap.toml`. Cyclomatic uses `new_function_ceiling`
//! from `code-metrics/function-complexity.toml`. A `waiver` key is rejected.

use std::path::Path;

use serde::Deserialize;

use super::model::Limits;

const CRAP_FILE: &str = ".cargo-crap.toml";
const COMPLEXITY_FILE: &str = "code-metrics/function-complexity.toml";

#[derive(Deserialize)]
struct CrapToml {
    threshold: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComplexityToml {
    new_function_ceiling: f64,
}

pub fn load(root: &Path) -> Result<Limits, Box<dyn std::error::Error>> {
    let crap: CrapToml = toml::from_str(&std::fs::read_to_string(root.join(CRAP_FILE))?)?;
    let complexity: ComplexityToml =
        toml::from_str(&std::fs::read_to_string(root.join(COMPLEXITY_FILE))?)?;
    if crap.threshold < 1.0 || complexity.new_function_ceiling < 1.0 {
        return Err("diff-gate limits must be at least 1".into());
    }
    Ok(Limits {
        crap: crap.threshold,
        cyclomatic: complexity.new_function_ceiling,
    })
}

#[cfg(test)]
mod tests {
    use super::load;
    use std::path::Path;

    fn write_pair(root: &Path, complexity: &str) {
        std::fs::write(root.join(".cargo-crap.toml"), "threshold = 29.9\n").unwrap();
        std::fs::create_dir_all(root.join("code-metrics")).unwrap();
        std::fs::write(
            root.join("code-metrics/function-complexity.toml"),
            complexity,
        )
        .unwrap();
    }

    #[test]
    fn the_repo_files_keep_the_approved_limits() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let limits = load(&root).expect("repo limits");
        assert!((limits.crap - 29.9).abs() < 1e-9);
        assert_eq!(limits.cyclomatic, 20.0);
        let crap = std::fs::read_to_string(root.join(".cargo-crap.toml")).unwrap();
        let complexity =
            std::fs::read_to_string(root.join("code-metrics/function-complexity.toml")).unwrap();
        assert!(crap.contains("threshold = 29.9"));
        assert!(complexity.contains("new_function_ceiling = 20"));
        assert!(!complexity.contains("[[waiver]]"));
    }

    #[test]
    fn a_waiver_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_pair(dir.path(), "new_function_ceiling = 20\nwaiver = []\n");
        let error = load(dir.path()).unwrap_err().to_string();
        assert!(
            error.contains("waiver") || error.contains("unknown"),
            "{error}"
        );
    }
}
