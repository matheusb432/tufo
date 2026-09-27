//! A value under the current user's `Run` registry key.

use std::{io, iter, ptr};

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
    System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    },
};

use super::AT_LOGIN_FLAG;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Tufo";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(iter::once(0)).collect()
}

fn checked(status: WIN32_ERROR) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status.cast_signed()))
    }
}

pub(crate) fn is_enabled() -> bool {
    let (key, value) = (wide(RUN_KEY), wide(VALUE_NAME));
    // SAFETY: both names are NUL-terminated, and null output pointers only ask whether it exists.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    status == ERROR_SUCCESS
}

pub(crate) fn set_enabled(enabled: bool) -> io::Result<()> {
    let (key, value) = (wide(RUN_KEY), wide(VALUE_NAME));
    if !enabled {
        // SAFETY: both names are NUL-terminated.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        return if status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            checked(status)
        };
    }
    let command = wide(&format!(
        "\"{}\" {AT_LOGIN_FLAG}",
        std::env::current_exe()?.display()
    ));
    let byte_len = u32::try_from(command.len() * size_of::<u16>())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the path is too long"))?;
    // SAFETY: the names and the command are NUL-terminated, and `byte_len` covers the command.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            REG_SZ,
            command.as_ptr().cast(),
            byte_len,
        )
    };
    checked(status)
}
