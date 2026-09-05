//! Contain an ACP child so cancel can stop the process tree, not only the direct child.

use crate::control_plane::ControlPlaneError;
use liberado_common::process::std_command;
use std::process::Stdio;

/// An ACP child plus the OS handle that owns its descendants.
#[derive(Debug)]
pub(super) struct ContainedProcess {
    child: std::process::Child,
    #[cfg(windows)]
    job: Option<WindowsJob>,
}

impl ContainedProcess {
    pub(super) fn spawn_acp(executable: &str, worktree: &str) -> Result<Self, ControlPlaneError> {
        let mut cmd = std_command(executable);
        cmd.arg("acp");
        cmd.current_dir(worktree);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        isolate_group(&mut cmd);
        contain(cmd.spawn().map_err(ControlPlaneError::Io)?)
    }

    pub(super) fn child_mut(&mut self) -> &mut std::process::Child {
        &mut self.child
    }

    pub(super) fn terminate(&mut self) {
        kill_descendants(self);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn contain(child: std::process::Child) -> Result<ContainedProcess, ControlPlaneError> {
    #[cfg(windows)]
    {
        match WindowsJob::assign(&child) {
            Ok(job) => Ok(ContainedProcess {
                child,
                job: Some(job),
            }),
            Err(error) => {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }
    #[cfg(not(windows))]
    Ok(ContainedProcess { child })
}

fn isolate_group(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
        // Born suspended so AssignProcessToJobObject wins the race against the
        // child's first CreateProcess. `start` / CREATE_BREAKAWAY_FROM_JOB is
        // still refused: the job does not set JOB_OBJECT_LIMIT_BREAKAWAY_OK.
        cmd.creation_flags(CREATE_SUSPENDED);
    }
}

fn kill_descendants(process: &mut ContainedProcess) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(process.child.id()) {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        process.job.take();
    }
}

/// Exclusive owner of a Windows job-object HANDLE.
///
/// The stored value is a kernel object identifier, not a pointer into process
/// memory. `OwnedHandle` is `Send + Sync`; `CloseHandle` runs only from `Drop`
/// (`&mut self`), so the job is closed once. Drop still applies
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and reaps descendants.
#[cfg(windows)]
#[derive(Debug)]
struct WindowsJob {
    _handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl WindowsJob {
    fn assign(child: &std::process::Child) -> Result<Self, ControlPlaneError> {
        use std::mem::{size_of, zeroed};
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(ControlPlaneError::Io(std::io::Error::last_os_error()));
        }
        // SAFETY: `CreateJobObjectW` returned a new exclusive kernel handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let job = handle.as_raw_handle();
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const information).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        let assigned = configured != 0
            && unsafe { AssignProcessToJobObject(job, child.as_raw_handle().cast()) } != 0;
        if assigned {
            resume_primary_thread(child.id())?;
            return Ok(Self { _handle: handle });
        }
        Err(ControlPlaneError::Io(std::io::Error::other(
            "could not contain the OpenCode process tree in a Windows job object",
        )))
    }
}

/// Resume the thread left paused by `CREATE_SUSPENDED` after the job owns it.
#[cfg(windows)]
fn resume_primary_thread(pid: u32) -> Result<(), ControlPlaneError> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return Err(ControlPlaneError::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: `CreateToolhelp32Snapshot` returned a new exclusive snapshot handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = unsafe { zeroed::<THREADENTRY32>() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle().cast(), &mut entry) };
    while more != 0 {
        if entry.th32OwnerProcessID == pid {
            return resume_thread(entry.th32ThreadID);
        }
        more = unsafe { Thread32Next(snapshot.as_raw_handle().cast(), &mut entry) };
    }
    Err(ControlPlaneError::Io(std::io::Error::other(
        "contained OpenCode process has no thread to resume",
    )))
}

#[cfg(windows)]
fn resume_thread(thread_id: u32) -> Result<(), ControlPlaneError> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() {
        return Err(ControlPlaneError::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: `OpenThread` returned a new exclusive thread handle.
    let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
    let previous = unsafe { ResumeThread(thread.as_raw_handle().cast()) };
    if previous == u32::MAX {
        return Err(ControlPlaneError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

const _: () = {
    const fn assert_send<T: Send>() {}
    const fn check() {
        assert_send::<ContainedProcess>();
    }
    check();
};

#[cfg(windows)]
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    const fn check() {
        assert_send_sync::<WindowsJob>();
    }
    check();
};
