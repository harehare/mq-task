//! Windows process-tree control: a Job Object per task, so that stopping a
//! task also stops what it started (the counterpart of `killpg` on Unix), and
//! Ctrl+Break delivery for a polite stop first.

use std::os::windows::io::AsRawHandle;
use std::process::Child;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

/// `CREATE_NEW_PROCESS_GROUP`: the task gets its own group, which does not
/// receive the console's Ctrl+C (mq-task forwards it) and can be sent
/// Ctrl+Break on its own.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// `CREATE_SUSPENDED`: the process is created with its main thread stopped,
/// so it can be put in its Job Object before it runs (and can start any
/// descendants that would otherwise escape the job).
pub const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// Let a process created with [`CREATE_SUSPENDED`] run: resume its threads
/// (the main thread is the only one so far). Returns whether one was resumed.
pub fn resume_process(pid: u32) -> bool {
    // SAFETY: plain API calls; the snapshot and thread handles are closed
    // before returning, and `entry` is initialised with its size as required.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = false;
        let mut more = Thread32First(snapshot, &mut entry) != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if !thread.is_null() {
                    resumed |= ResumeThread(thread) != u32::MAX;
                    CloseHandle(thread);
                }
            }
            more = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        resumed
    }
}

/// A Job Object holding a task's processes.
pub struct Job(HANDLE);

// SAFETY: a job handle is just a kernel object reference; the calls made on
// it are thread-safe.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// Create a job and put `child` in it. `None` when either step fails
    /// (for instance the process already belongs to a job that forbids
    /// nesting); callers then fall back to killing the direct child.
    pub fn with_child(child: &Child) -> Option<Job> {
        // SAFETY: null attributes and name are valid arguments.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return None;
        }
        let job = Job(handle);
        // SAFETY: both handles are valid for the duration of the call.
        let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) };
        (assigned != 0).then_some(job)
    }

    /// Terminate every process in the job.
    pub fn terminate(&self) {
        // SAFETY: valid job handle.
        unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Closing does not kill the processes (no kill-on-close limit is
        // set): a task that deliberately leaves something running keeps it,
        // as on Unix.
        // SAFETY: the handle is owned by this value.
        unsafe { CloseHandle(self.0) };
    }
}

/// Send Ctrl+Break to the process group led by `pid`. Returns whether the
/// event was delivered (it is not when there is no console).
pub fn ctrl_break(pid: u32) -> bool {
    // SAFETY: plain API call.
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 }
}
