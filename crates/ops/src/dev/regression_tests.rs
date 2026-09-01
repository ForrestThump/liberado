use super::*;

#[test]
fn detached_spawn_writes_a_process_record_and_missing_stop_is_safe() {
    let repository = tempfile::tempdir().unwrap();
    spawn_detached(
        repository.path(),
        "probe",
        Path::new("rustc"),
        &["--version".into()],
        &[],
    )
    .unwrap();
    let record = process_file(repository.path(), "probe");
    assert!(record.is_file());
    let decoded: ProcessRecord = serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
    assert_eq!(decoded.program, "rustc");

    stop_process(repository.path(), "absent").unwrap();
}
