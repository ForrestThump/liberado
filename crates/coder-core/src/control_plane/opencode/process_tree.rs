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
    #[cfg(not(unix))]
    {
        let _ = cmd;
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

#[cfg(windows)]
#[derive(Debug)]
struct WindowsJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl WindowsJob {
    fn assign(child: &std::process::Child) -> Result<Self, ControlPlaneError> {
        use std::mem::{size_of, zeroed};
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(ControlPlaneError::Io(std::io::Error::last_os_error()));
        }
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const information).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        let assigned = configured != 0
            && unsafe { AssignProcessToJobObject(handle, child.as_raw_handle().cast()) } != 0;
        if assigned {
            return Ok(Self { handle });
        }
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        Err(ControlPlaneError::Io(std::io::Error::other(
            "could not contain the OpenCode process tree in a Windows job object",
        )))
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
        }
    }
}
