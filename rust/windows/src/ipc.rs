//! Private, bounded local IPC for owned user-session helpers.
use anyhow::{Context, Result, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::Win32::{
    Foundation::*,
    Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, PSECURITY_DESCRIPTOR,
        SECURITY_ATTRIBUTES,
    },
    Storage::FileSystem::*,
    System::{Com::CoCreateGuid, Pipes::*},
};
use windows::core::{BOOL, HRESULT, PCWSTR};

pub(crate) const MESSAGE_LIMIT: usize = 4096;

fn owned(handle: HANDLE) -> OwnedHandle {
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}
fn utf16(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}
pub(crate) struct Pipe(OwnedHandle);
impl Pipe {
    pub(crate) fn server(prefix: &str) -> Result<(Self, String)> {
        let sid = crate::process::user_sid().context("helper needs a signed-in user")?;
        // The only client is the signed-in user. No anonymous/network access;
        // a PID check below also rejects another process under that same user.
        let sddl = utf16(&format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{sid})"));
        let mut descriptor = Descriptor(PSECURITY_DESCRIPTOR::default());
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut descriptor.0,
                None,
            )?;
            let name = format!("{prefix}{:?}", CoCreateGuid()?);
            let wide = utf16(&name);
            let security = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0.0,
                bInheritHandle: BOOL(0),
            };
            let handle = CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_MESSAGE
                    | PIPE_READMODE_MESSAGE
                    | PIPE_NOWAIT
                    | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                16 * 1024,
                16 * 1024,
                0,
                Some(&security),
            );
            ensure!(
                !handle.is_invalid(),
                "create helper pipe: {}",
                windows::core::Error::from_thread()
            );
            Ok((Self(owned(handle)), name))
        }
    }
    pub(crate) fn client(name: &str, parent: u32, prefix: &str) -> Result<Self> {
        ensure!(
            name.starts_with(prefix) && name.len() < 128 && !name.contains('\0'),
            "invalid helper pipe name"
        );
        let name = utf16(name);
        unsafe {
            let pipe = Self(owned(CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )?));
            let mut actual = 0;
            GetNamedPipeServerProcessId(raw(&pipe.0), &mut actual)?;
            ensure!(
                actual == parent && parent != 0,
                "helper pipe server identity mismatch"
            );
            SetNamedPipeHandleState(
                raw(&pipe.0),
                Some(&(PIPE_READMODE_MESSAGE | PIPE_NOWAIT)),
                None,
                None,
            )?;
            Ok(pipe)
        }
    }
    pub(crate) fn connected(&self, expected_pid: u32) -> Result<bool> {
        unsafe {
            if let Err(error) = ConnectNamedPipe(raw(&self.0), None) {
                if error.code() == HRESULT::from_win32(ERROR_PIPE_LISTENING.0) {
                    return Ok(false);
                }
                if error.code() != HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) {
                    return Err(error.into());
                }
            }
            let mut actual = 0;
            GetNamedPipeClientProcessId(raw(&self.0), &mut actual)?;
            ensure!(
                actual == expected_pid && expected_pid != 0,
                "helper pipe client identity mismatch"
            );
            Ok(true)
        }
    }
    pub(crate) fn send(&self, message: &impl Serialize) -> Result<()> {
        let bytes = serde_json::to_vec(message)?;
        ensure!(
            bytes.len() <= MESSAGE_LIMIT,
            "helper message exceeds its bound"
        );
        let mut written = 0;
        unsafe {
            WriteFile(raw(&self.0), Some(&bytes), Some(&mut written), None)?;
        }
        // Nonblocking message writes either fit in full or fail. Never block a
        // capture/teardown thread if the peer stalls or exits.
        ensure!(written as usize == bytes.len(), "helper pipe is full");
        Ok(())
    }
    pub(crate) fn receive<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        let mut size = 0;
        unsafe {
            PeekNamedPipe(raw(&self.0), None, 0, None, None, Some(&mut size))?;
            if size == 0 {
                return Ok(None);
            }
            ensure!(size as usize <= MESSAGE_LIMIT, "oversized helper message");
            let mut bytes = vec![0; size as usize];
            let mut read = 0;
            ReadFile(raw(&self.0), Some(&mut bytes), Some(&mut read), None)?;
            ensure!(read == size, "incomplete helper message");
            Ok(Some(serde_json::from_slice(&bytes)?))
        }
    }
}
