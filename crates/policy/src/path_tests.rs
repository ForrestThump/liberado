use super::*;

fn zones(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn relative_zone_path_normalizes() {
    let path = normalize_vault_path("tasks/notes/a.md", &[]).unwrap();
    assert_eq!(path.zone(), "tasks");
    assert_eq!(path.rest(), "notes/a.md");
    assert_eq!(path.as_relative(), "tasks/notes/a.md");
}

#[test]
fn backslash_separators_normalize_to_slash() {
    let path = normalize_vault_path("tasks\\notes\\a.md", &[]).unwrap();
    assert_eq!(path.as_relative(), "tasks/notes/a.md");
}

#[test]
fn dot_and_empty_segments_drop() {
    let path = normalize_vault_path("tasks/./notes//a.md", &[]).unwrap();
    assert_eq!(path.as_relative(), "tasks/notes/a.md");
}

#[test]
fn parent_segment_is_refused() {
    assert_eq!(
        normalize_vault_path("tasks/../secret.md", &[]),
        Err(PathError::ParentSegment)
    );
    assert_eq!(
        normalize_vault_path("tasks/ .. /secret.md", &[]),
        Err(PathError::ParentSegment)
    );
    assert_eq!(
        normalize_vault_path("tasks\\..\\secret.md", &[]),
        Err(PathError::ParentSegment)
    );
}

#[test]
fn absolute_path_is_refused() {
    assert_eq!(
        normalize_vault_path("/tasks/a.md", &[]),
        Err(PathError::Absolute)
    );
    assert_eq!(
        normalize_vault_path("\\\\server\\share\\a.md", &[]),
        Err(PathError::Absolute)
    );
}

#[test]
fn drive_letter_is_refused() {
    assert_eq!(
        normalize_vault_path("C:\\tasks\\a.md", &[]),
        Err(PathError::DriveLetter)
    );
    assert_eq!(
        normalize_vault_path("tasks/C:/a.md", &[]),
        Err(PathError::DriveLetter)
    );
    assert_eq!(
        normalize_vault_path("C:tasks/a.md", &[]),
        Err(PathError::DriveLetter)
    );
}

#[test]
fn control_character_is_refused() {
    assert_eq!(
        normalize_vault_path("tasks/a\u{0000}.md", &[]),
        Err(PathError::ControlCharacter)
    );
}

#[test]
fn zone_case_mismatch_is_refused() {
    let error = normalize_vault_path("Tasks/a.md", &zones(&["tasks"])).unwrap_err();
    assert_eq!(
        error,
        PathError::ZoneCaseMismatch {
            found: "Tasks".into(),
            expected: "tasks".into(),
        }
    );
}

#[test]
fn exact_zone_case_is_accepted() {
    let path = normalize_vault_path("Tasks/a.md", &zones(&["Tasks"])).unwrap();
    assert_eq!(path.zone(), "Tasks");
}

#[test]
fn open_scope_keeps_the_caller_zone_case() {
    let path = normalize_vault_path("Tasks/a.md", &[]).unwrap();
    assert_eq!(path.zone(), "Tasks");
}

#[test]
fn undeclared_zone_is_refused() {
    let error = normalize_vault_path("finance/a.md", &zones(&["tasks"])).unwrap_err();
    assert_eq!(
        error,
        PathError::ZoneNotDeclared {
            found: "finance".into()
        }
    );
}

#[test]
fn bare_filename_names_no_zone() {
    assert_eq!(
        normalize_vault_path("loose.md", &[]),
        Err(PathError::NoZone)
    );
    assert_eq!(normalize_vault_path("tasks", &[]), Err(PathError::NoZone));
}

#[test]
fn empty_path_is_refused() {
    assert_eq!(normalize_vault_path("", &[]), Err(PathError::Empty));
}

#[test]
fn a_declared_zone_must_be_one_segment() {
    assert_eq!(validate_zone_name("tasks/a"), Err(PathError::BadZone));
    assert_eq!(validate_zone_name(".."), Err(PathError::BadZone));
    assert!(validate_zone_name("tasks").is_ok());
}
