//! OpenCode process-tree cancel coverage, split from `control_plane_tests.rs`.

use super::control_plane::*;

#[test]
fn opencode_cancel_stops_descendant_work() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = temp.path();
    init_git_repo(path);
    let marker = path.join("descendant.marker");
    let executable = write_descendant_worker(path, &marker);

    let worker = OpenCodeWorker::new(OpenCodeWorkerConfig {
        executable: Some(executable.to_string_lossy().into_owned()),
        ..OpenCodeWorkerConfig::default()
    });
    let request = WorkerRunRequest {
        task_id: "task-descendants".into(),
        objective: "stop the tree".into(),
        worktree: path.to_string_lossy().into_owned(),
        branch: "master".into(),
        base_ref: "HEAD".into(),
        prompt: "wait".into(),
        session_id: None,
    };
    let handle = worker.start(&request).expect("start worker");
    wait_for_marker(&marker, 3);
    let result = cancel_and_collect_within(worker, handle);
    assert_eq!(result.status, WorkerStatus::Failed);
    assert_eq!(
        result.blocking_issue.as_deref(),
        Some("worker run was cancelled")
    );

    std::thread::sleep(std::time::Duration::from_millis(300));
    let after_cancel = marker_lines(&marker);
    std::thread::sleep(std::time::Duration::from_millis(400));
    let later = marker_lines(&marker);
    assert!(
        after_cancel >= 3,
        "descendant must have started writing before cancel"
    );
    assert_eq!(
        after_cancel, later,
        "killing only the direct child leaves descendant work running"
    );
}

fn cancel_and_collect_within(worker: OpenCodeWorker, handle: RunHandle) -> WorkerRunResult {
    let worker = std::sync::Arc::new(worker);
    let worker_for_thread = worker.clone();
    let handle_for_thread = handle.clone();
    let join = std::thread::spawn(move || {
        worker_for_thread
            .cancel(&handle_for_thread)
            .expect("cancel worker");
        worker_for_thread
            .collect(&handle_for_thread)
            .expect("collect cancellation")
    });
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(3) {
        if join.is_finished() {
            return join.join().expect("cancel thread");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("cancel must stop the process tree instead of waiting for descendant work");
}

fn init_git_repo(path: &std::path::Path) {
    for args in [
        vec!["init"],
        vec!["config", "user.name", "Test Agent"],
        vec!["config", "user.email", "agent@test.local"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git setup")
                .success()
        );
    }
    std::fs::write(path.join("README.md"), "test").expect("write fixture");
    for args in [vec!["add", "README.md"], vec!["commit", "-m", "initial"]] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git commit")
                .success()
        );
    }
}

fn write_descendant_worker(path: &std::path::Path, marker: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        assert_eq!(
            marker.file_name().and_then(|name| name.to_str()),
            Some("descendant.marker"),
            "Windows fixture writes the worktree-relative marker name"
        );
        std::fs::write(
            path.join("descendant.cmd"),
            windows_descendant_writer_script(),
        )
        .expect("write descendant writer");
        let script = path.join("fake-opencode.cmd");
        std::fs::write(&script, windows_fake_opencode_script()).expect("write fake ACP server");
        script
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = path.join("fake-opencode");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nmarker='{}'\n( while :; do printf 'alive\\n' >> \"$marker\"; sleep 0.1; done ) &\nsleep 60\n",
                marker.display()
            ),
        )
        .expect("write fake ACP server");
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        script
    }
}

/// Parent ACP fake: spawn a child `cmd` that inherits the job, then wait.
///
/// `start` (including `start /b` and `start "" /min`) asks for a new console
/// and often `CREATE_BREAKAWAY_FROM_JOB`. The job refuses breakaway, so that
/// writer never starts — or it starts outside the job and cancel misses it.
/// A nested `cmd /c` is an ordinary child and stays contained.
fn windows_fake_opencode_script() -> &'static str {
    "@echo off\r\ncmd /c descendant.cmd\r\n"
}

/// Descendant writer: three lines immediately, then keep writing until killed.
fn windows_descendant_writer_script() -> &'static str {
    "@echo off\r\n\
     echo alive>>\"descendant.marker\"\r\n\
     echo alive>>\"descendant.marker\"\r\n\
     echo alive>>\"descendant.marker\"\r\n\
     :loop\r\n\
     echo alive>>\"descendant.marker\"\r\n\
     ping 192.0.2.1 -n 1 -w 100 >nul\r\n\
     goto loop\r\n"
}

#[test]
fn windows_descendant_fixture_stays_inside_the_job() {
    let parent = windows_fake_opencode_script();
    let writer = windows_descendant_writer_script();
    assert!(
        !parent.to_ascii_lowercase().contains("start"),
        "start breakaway-from-job either fails or escapes the job: {parent}"
    );
    assert!(
        parent.contains("cmd /c descendant.cmd"),
        "parent must spawn a nested cmd child: {parent}"
    );
    assert!(
        writer.contains("descendant.marker"),
        "writer must use the worktree-relative marker: {writer}"
    );
    assert!(
        !writer.contains(":\\"),
        "absolute Windows paths break inside cmd quoting: {writer}"
    );
}

fn wait_for_marker(path: &std::path::Path, min_lines: usize) {
    let started = std::time::Instant::now();
    while marker_lines(path) < min_lines {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "descendant never started writing to {}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn marker_lines(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}
