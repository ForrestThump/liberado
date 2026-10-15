//! Direct calls to [`replace_existing`](super::replace_existing).
//!
//! Linux `rename` replaces a file, so the production write never enters this
//! path there. These tests run on every OS.

use std::path::PathBuf;

use liberado_conversation_store::Ulid;

use super::{replace_existing, tmp_path};

fn scratch(tag: &str) -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("liberado-replace-{tag}-{}", Ulid::new()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

#[tokio::test]
async fn replace_existing_swaps_a_file_that_is_already_there() -> std::io::Result<()> {
    let dir = scratch("present")?;
    let path = dir.join("channel-bindings.json");
    let tmp = tmp_path(&path);
    std::fs::write(&path, "old")?;
    std::fs::write(&tmp, "new")?;
    replace_existing(&tmp, &path, std::io::Error::other("ignored")).await?;
    let body = std::fs::read_to_string(&path)?;
    if body != "new" {
        return Err(std::io::Error::other(format!("replaced body was {body}")));
    }
    if tmp.exists() {
        return Err(std::io::Error::other(
            "tmp file must be gone after the rename",
        ));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[tokio::test]
async fn replace_existing_renames_when_the_destination_is_missing() -> std::io::Result<()> {
    let dir = scratch("missing")?;
    let path = dir.join("channel-bindings.json");
    let tmp = tmp_path(&path);
    std::fs::write(&tmp, "new")?;
    if path.exists() {
        return Err(std::io::Error::other("destination must start missing"));
    }
    replace_existing(&tmp, &path, std::io::Error::other("ignored")).await?;
    let body = std::fs::read_to_string(&path)?;
    if body != "new" {
        return Err(std::io::Error::other(format!("renamed body was {body}")));
    }
    if tmp.exists() {
        return Err(std::io::Error::other(
            "tmp file must be gone after the rename",
        ));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[tokio::test]
async fn replace_existing_returns_the_original_error_when_remove_fails() -> std::io::Result<()> {
    let dir = scratch("occupied")?;
    let path = dir.join("occupied");
    std::fs::create_dir(&path)?;
    std::fs::write(path.join("child"), "keep")?;
    let tmp = tmp_path(&path);
    std::fs::write(&tmp, "new")?;
    let error = match replace_existing(
        &tmp,
        &path,
        std::io::Error::new(std::io::ErrorKind::AlreadyExists, "rename failed"),
    )
    .await
    {
        Err(error) => error,
        Ok(()) => {
            return Err(std::io::Error::other(
                "remove of a non-empty directory must fail",
            ));
        }
    };
    if error.kind() != std::io::ErrorKind::AlreadyExists || error.to_string() != "rename failed" {
        return Err(std::io::Error::other(format!(
            "original error was not returned: {error}"
        )));
    }
    if tmp.exists() {
        return Err(std::io::Error::other(
            "tmp file must be removed when replace fails",
        ));
    }
    if !path.join("child").is_file() {
        return Err(std::io::Error::other("occupied directory must stay"));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
