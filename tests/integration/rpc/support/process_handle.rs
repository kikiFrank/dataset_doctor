//! Hold exactly the synthetic descendant created by a test, with cleanup on panic.
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

pub struct SyntheticProcess(OwnedHandle);

impl SyntheticProcess {
    pub fn open(pid: u32) -> Self {
        // SAFETY: pid came from this test's private worker marker. The returned
        // handle has only synchronize/terminate rights and is owned here.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        assert!(!raw.is_null(), "open synthetic descendant");
        Self(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
    pub fn stopped(&self) -> bool {
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 2000) == WAIT_OBJECT_0 }
    }
}

impl Drop for SyntheticProcess {
    fn drop(&mut self) {
        // SAFETY: a held process handle cannot be redirected by PID reuse.
        unsafe {
            TerminateProcess(self.0.as_raw_handle(), 99);
            WaitForSingleObject(self.0.as_raw_handle(), 2000);
        }
    }
}
