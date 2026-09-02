//! Split from `journal.rs` for module-health boundaries.

use super::tests::spec;
use super::*;
use std::fs;
use std::io;

#[test]
fn job_count_is_zero_when_the_root_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    assert_eq!(store.job_count().unwrap(), 0);
}

#[test]
fn job_count_counts_job_directories_only() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    store.create(&spec()).unwrap();
    store.create(&spec()).unwrap();
    fs::write(store.root().join("not-a-job"), "x").unwrap();
    fs::create_dir(store.root().join("not-a-ulid")).unwrap();
    assert_eq!(store.job_count().unwrap(), 2);
}

#[test]
fn acquire_lease_is_exclusive_while_the_holder_is_alive() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    let spec = spec();
    store.create(&spec).unwrap();
    let held = store.acquire_lease(&spec.job_id).unwrap();
    match store.acquire_lease(&spec.job_id) {
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::AlreadyExists),
        Ok(_) => panic!("a live holder must keep the lease exclusive"),
    }
    drop(held);
    store
        .acquire_lease(&spec.job_id)
        .expect("a dropped lease can be reacquired");
}

#[test]
fn acquire_lease_replaces_a_dead_holder() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    let spec = spec();
    store.create(&spec).unwrap();
    fs::write(
        store.job_root(&spec.job_id).join("worker.lease"),
        "pid=999999999\n",
    )
    .unwrap();
    store
        .acquire_lease(&spec.job_id)
        .expect("a lease whose pid is dead is stolen");
}
