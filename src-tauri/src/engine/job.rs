//! Process-tree ownership for the worker.
//!
//! Windows: the worker is put in a Job Object with KILL_ON_JOB_CLOSE, so
//! terminating the job (cancel) or dropping it (also when the app crashes)
//! ends the worker and everything it started (`uv run` spawns the real Python).
//! Elsewhere: plain `Child::kill`.

use std::process::Child;

pub struct ProcessTree {
    #[cfg(windows)]
    job: Option<win::Job>,
}

impl ProcessTree {
    /// Take ownership of `child`'s process tree. Never fails: without a job we fall back to `Child::kill`.
    pub fn adopt(child: &Child) -> Self {
        #[cfg(windows)]
        {
            Self { job: win::Job::for_child(child) }
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Self {}
        }
    }

    pub fn kill(&mut self, child: &mut Child) {
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            job.terminate();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(windows)]
mod win {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // The handle is only used through the owning `ProcessTree`, behind the engine mutex.
    unsafe impl Send for Job {}

    impl Job {
        pub fn for_child(child: &Child) -> Option<Self> {
            // SAFETY: plain Win32 calls on a handle we own; every failure path closes it.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let job = Job(handle);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 || AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) == 0 {
                    return None; // `job` drops and closes the handle
                }
                Some(job)
            }
        }

        pub fn terminate(self) {
            // SAFETY: valid job handle owned by self; Drop closes it afterwards.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closing our own handle; with KILL_ON_JOB_CLOSE this also ends the tree.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
