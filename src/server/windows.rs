//! Windows Command inherits all inheritable handles, including the CLI's old
//! stdout pipe even when the daemon's stdout is redirected to a log. Keep that
//! pipe out of the detached child so callers waiting for EOF can return.
use std::io;
use windows_sys::Win32::Foundation::{
    GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

pub struct InheritanceGuard(Vec<(HANDLE, u32)>);

impl InheritanceGuard {
    pub fn disable_stdio() -> io::Result<Self> {
        let mut guard = Self(Vec::new());
        for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // These handles are borrowed from the process, never closed here.
            let handle = unsafe { GetStdHandle(stream) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut flags = 0;
            if unsafe { GetHandleInformation(handle, &mut flags) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if flags & HANDLE_FLAG_INHERIT != 0 {
                if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                guard.0.push((handle, flags));
            }
        }
        Ok(guard)
    }
}

impl Drop for InheritanceGuard {
    fn drop(&mut self) {
        for (handle, flags) in &self.0 {
            // Restore only the inheritance bit changed before spawning. This
            // CLI command spawns no other child concurrently with the daemon.
            unsafe {
                SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, flags & HANDLE_FLAG_INHERIT);
            }
        }
    }
}
