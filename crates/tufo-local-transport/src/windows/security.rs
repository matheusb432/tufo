//! The current user's SID and pipes whose DACL admits only that user.

use std::{
    ffi::c_void,
    io,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    ptr,
};

use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: this pointer is allocated by a Windows conversion function using LocalAlloc.
        unsafe {
            LocalFree(self.0);
        }
    }
}

/// The current process user's SID in its `S-1-5-...` string form.
pub(super) fn current_user_sid() -> io::Result<String> {
    let mut token = ptr::null_mut();
    // SAFETY: the process pseudo-handle is valid and token points to writable handle storage.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a newly owned handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut required = 0;
    // SAFETY: a null buffer and zero size query the required token buffer size.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &raw mut required,
        );
    }
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
    // SAFETY: the word-aligned buffer holds at least required bytes and the token remains open.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &raw mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful TokenUser query initializes TOKEN_USER at the aligned buffer start.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = ptr::null_mut();
    // SAFETY: the SID is backed by the live token-information buffer and text is writable.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalAllocation(text.cast());
    // SAFETY: the conversion returns a NUL-terminated UTF-16 string retained by _allocation.
    let length = unsafe { (0..184).find(|index| *text.add(*index) == 0) }.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows SID string exceeds its maximum length",
        )
    })?;
    // SAFETY: length ends at the terminator inside the live allocation.
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Creates one pipe instance that only `sid` may open, refusing remote clients. `first` makes
/// creation fail while another process owns the name.
pub(super) fn create_pipe(
    name: &str,
    sid: &str,
    first: bool,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let descriptor_text = format!("D:P(A;;GA;;;{sid})\0")
        .encode_utf16()
        .collect::<Vec<_>>();
    let mut descriptor = ptr::null_mut();
    // SAFETY: descriptor_text is NUL-terminated and descriptor receives an owned allocation.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            1,
            &raw mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalAllocation(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(io::Error::other)?,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: attributes and its security descriptor remain valid until pipe creation returns.
    unsafe {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
    }
}
