//! A device opened by path for `DeviceIoControl`: HidHide's control device
//! (`hidhide`) and the Xbox 360 driver's interfaces (`xinput_slots`).

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::IO::DeviceIoControl,
    },
    core::HSTRING,
};

/// An open device; closed on drop.
pub(crate) struct DeviceFile(HANDLE);

impl Drop for DeviceFile {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateFileW` and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl DeviceFile {
    /// Opens `path` with `access` (`GENERIC_READ`'s bits and so on), shared
    /// for reading and writing with whoever else has it open.
    pub(crate) fn open(path: &HSTRING, access: u32) -> windows::core::Result<Self> {
        // SAFETY: `path` is a NUL-terminated wide string that outlives the
        // call; no security attributes or template.
        let handle = unsafe {
            CreateFileW(
                path,
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }?;
        Ok(Self(handle))
    }

    /// Sends `code` with `input`, the answer going into `output`. Returns
    /// the byte count the driver gives, which may be more than `output`
    /// holds: HidHide answers an empty `output` with the size its list
    /// takes. Callers reading `output` cut the count to its length.
    pub(crate) fn ioctl(
        &self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> windows::core::Result<usize> {
        let mut returned = 0u32;
        // SAFETY: both buffers are valid for the lengths passed (null when
        // empty), and the call is synchronous (no OVERLAPPED), so they
        // outlive it.
        unsafe {
            DeviceIoControl(
                self.0,
                code,
                (!input.is_empty()).then_some(input.as_ptr().cast()),
                input.len() as u32,
                (!output.is_empty()).then_some(output.as_mut_ptr().cast()),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        }?;
        Ok(returned as usize)
    }
}
