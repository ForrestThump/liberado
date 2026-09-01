use super::*;

pub(super) fn vacated_image_destination(exe: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dest_dir = repository_root()?.join(".liberado");
    std::fs::create_dir_all(&dest_dir)?;
    Ok(match exe.extension() {
        Some(ext) => dest_dir.join(VACATED_BIN).with_extension(ext),
        None => dest_dir.join(VACATED_BIN),
    })
}

pub(super) fn move_running_image(
    exe: &Path,
    dest: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = std::fs::remove_file(dest);
    std::fs::rename(exe, dest).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "could not move running image from {} to {}: {error}",
                exe.display(),
                dest.display()
            ),
        )
    })?;
    eprintln!(
        "[liberado ci] moved running image to {} so cargo can rebuild it",
        dest.display()
    );
    Ok(())
}

pub(super) fn announce_staged_baseline(outcome: StageOutcome) {
    match outcome {
        StageOutcome::Unchanged => eprintln!("[liberado ci] {BASELINE_FILE} unchanged"),
        StageOutcome::Staged => eprintln!(
            "[liberado ci] staged {BASELINE_FILE}; other dirty files present — not amending"
        ),
        StageOutcome::Amended => {
            eprintln!("[liberado ci] amended {BASELINE_FILE} onto HEAD");
        }
    }
}
