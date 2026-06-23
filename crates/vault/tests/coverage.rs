//! Integration coverage for `liberado-vault` against real Turbovault: the typed `Conflict`
//! adapter seam (§8.1), nested writes, and delete/move provenance/attribution — the operations
//! the loop-breaking unit tests don't exercise.

use liberado_vault::{Attribution, Vault, VaultError, WriteProvenance};
use tempfile::TempDir;

async fn temp_vault() -> (Vault, TempDir) {
    let dir = TempDir::new().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    (vault, dir)
}

#[tokio::test]
async fn write_creates_nested_dirs_and_reads_back() {
    let (vault, _dir) = temp_vault().await;
    let prov = WriteProvenance::agent("a", "c1");
    vault
        .write("a/deeply/nested/note.md", "# nested\nbody", None, &prov)
        .await
        .unwrap();
    assert_eq!(
        vault.read("a/deeply/nested/note.md").await.unwrap(),
        "# nested\nbody"
    );
}

#[tokio::test]
async fn stale_expected_hash_yields_typed_conflict() {
    let (vault, _dir) = temp_vault().await;
    let prov = WriteProvenance::agent("a", "c1");
    vault.write("n.md", "v1", None, &prov).await.unwrap();

    // A write guarded by a hash that doesn't match current content must surface as a typed
    // Conflict (the §8.1 isolation point), not a generic backend error.
    let stale = Vault::content_hash("some other content");
    let err = vault
        .write("n.md", "v2", Some(&stale), &prov)
        .await
        .unwrap_err();
    assert!(matches!(err, VaultError::Conflict(_)), "got {err:?}");

    // The note is unchanged after a rejected write.
    assert_eq!(vault.read("n.md").await.unwrap(), "v1");
}

#[tokio::test]
async fn correct_expected_hash_allows_write() {
    let (vault, _dir) = temp_vault().await;
    let prov = WriteProvenance::agent("a", "c1");
    vault.write("n.md", "v1", None, &prov).await.unwrap();

    let current = Vault::content_hash("v1");
    vault
        .write("n.md", "v2", Some(&current), &prov)
        .await
        .unwrap();
    assert_eq!(vault.read("n.md").await.unwrap(), "v2");
}

#[tokio::test]
async fn delete_removes_note_and_attributes_missing() {
    let (vault, _dir) = temp_vault().await;
    let prov = WriteProvenance::agent("cleaner", "del-1");
    vault.write("x.md", "bye", None, &prov).await.unwrap();

    vault.delete("x.md", None, &prov).await.unwrap();
    assert!(vault.read("x.md").await.is_err());
    assert_eq!(vault.attribute("x.md").await.unwrap(), Attribution::Missing);
}

#[tokio::test]
async fn move_attributes_at_new_path_and_clears_old() {
    let (vault, _dir) = temp_vault().await;
    let prov = WriteProvenance::agent("mover", "mv-1");
    vault
        .write("from.md", "content", None, &prov)
        .await
        .unwrap();

    vault
        .move_note("from.md", "to.md", None, &prov)
        .await
        .unwrap();

    // The moved note's current bytes are attributed to our move at the new path...
    match vault.attribute("to.md").await.unwrap() {
        Attribution::Agent(p) => {
            assert_eq!(p.source, "mover");
            assert_eq!(p.correlation_id.as_deref(), Some("mv-1"));
        }
        other => panic!("expected Agent at new path, got {other:?}"),
    }
    // ...and the old path no longer exists.
    assert_eq!(
        vault.attribute("from.md").await.unwrap(),
        Attribution::Missing
    );
}
