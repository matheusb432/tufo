//! A value under the current user's `Run` registry key.

use std::{io, iter, path::Path, ptr};

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

pub(super) fn read_enabled() -> io::Result<bool> {
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
    match status {
        ERROR_SUCCESS => Ok(true),
        ERROR_FILE_NOT_FOUND => Ok(false),
        _ => Err(io::Error::from_raw_os_error(status.cast_signed())),
    }
}

pub(super) fn disable() -> io::Result<()> {
    let (key, value) = (wide(RUN_KEY), wide(VALUE_NAME));
    // SAFETY: both names are NUL-terminated.
    let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
    if status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        checked(status)
    }
}

pub(super) fn enable(gui: &Path) -> io::Result<()> {
    let (key, value) = (wide(RUN_KEY), wide(VALUE_NAME));
    let command = wide(&command_text(gui));
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

fn command_text(gui: &Path) -> String {
    format!("\"{}\" {AT_LOGIN_FLAG}", gui.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_value_launches_the_gui_at_login() {
        assert_eq!(
            command_text(Path::new(r"C:\Program Files\Tufo\tufo-gui.exe")),
            r#""C:\Program Files\Tufo\tufo-gui.exe" --at-login"#,
        );
    }
}
