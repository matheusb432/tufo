//! On Windows, a private token file's DACL lets only its owner, the current user, SYSTEM, and
//! Administrators read it. A file in the user's profile inherits exactly those entries.

use std::{
    ffi::c_void,
    io, iter,
    os::windows::{
        ffi::OsStrExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    },
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, GENERIC_ALL, GENERIC_READ, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
        CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
        GetTokenInformation, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSID,
        SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser, WELL_KNOWN_SID_TYPE,
        WinBuiltinAdministratorsSid, WinLocalSystemSid,
    },
    Storage::FileSystem::FILE_READ_DATA,
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

use super::LoadCredentialsError;

/// Rights that let a principal read the file's contents.
const READ_ACCESS_RIGHTS: u32 = FILE_READ_DATA | GENERIC_READ | GENERIC_ALL;
/// A SID buffer of `SECURITY_MAX_SID_SIZE` bytes, aligned for the SID's 32-bit sub-authorities.
const SID_BUFFER_WORDS: usize = SECURITY_MAX_SID_SIZE as usize / size_of::<u32>();

/// Creates a new file. It inherits the folder's permissions; Tufo's config folder in the user's
/// profile grants access only to the user, SYSTEM, and Administrators, which
/// [`ensure_owner_only`] then confirms.
pub(super) fn create_owner_only(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

pub(super) fn ensure_owner_only(path: &Path) -> Result<(), LoadCredentialsError> {
    std::fs::metadata(path).map_err(LoadCredentialsError::Read)?;
    let security = FileSecurity::read(path).map_err(LoadCredentialsError::Permissions)?;
    let current_user = CurrentUser::query().map_err(LoadCredentialsError::Permissions)?;
    let system = well_known_sid(WinLocalSystemSid).map_err(LoadCredentialsError::Permissions)?;
    let administrators =
        well_known_sid(WinBuiltinAdministratorsSid).map_err(LoadCredentialsError::Permissions)?;
    let trusted_readers = [
        security.owner,
        current_user.sid(),
        system.as_ptr().cast_mut().cast(),
        administrators.as_ptr().cast_mut().cast(),
    ];
    match security.grants_read_to_others(&trusted_readers) {
        Ok(false) => Ok(()),
        Ok(true) => Err(LoadCredentialsError::Exposed),
        Err(error) => Err(LoadCredentialsError::Permissions(error)),
    }
}

/// A file's owner and DACL, inside the security descriptor Windows allocated for them.
struct FileSecurity {
    descriptor: *mut c_void,
    owner: PSID,
    dacl: *const ACL,
}

impl FileSecurity {
    fn read(path: &Path) -> io::Result<Self> {
        let wide_path = path
            .as_os_str()
            .encode_wide()
            .chain(iter::once(0))
            .collect::<Vec<_>>();
        let mut owner = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        // SAFETY: the path is NUL-terminated and every out-pointer refers to writable storage.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut owner,
                ptr::null_mut(),
                &raw mut dacl,
                ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.cast_signed()));
        }
        Ok(Self {
            descriptor,
            owner,
            dacl,
        })
    }

    /// Whether an entry that applies to the file lets anyone outside `trusted_readers` read it.
    /// A null DACL grants everyone full access. Allow entries of kinds files don't normally carry
    /// count as exposing the file, because only the plain kind is parsed.
    fn grants_read_to_others(&self, trusted_readers: &[PSID]) -> io::Result<bool> {
        if self.dacl.is_null() {
            return Ok(true);
        }
        let mut size = ACL_SIZE_INFORMATION::default();
        // SAFETY: the DACL lives in the descriptor and `size` is writable for its full length.
        if unsafe {
            GetAclInformation(
                self.dacl,
                (&raw mut size).cast(),
                u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).map_err(io::Error::other)?,
                AclSizeInformation,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        for index in 0..size.AceCount {
            let mut entry = ptr::null_mut();
            // SAFETY: `index` is below the DACL's entry count and `entry` is writable.
            if unsafe { GetAce(self.dacl, index, &raw mut entry) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: every ACE starts with its header.
            let header = unsafe { &*entry.cast::<ACE_HEADER>() };
            if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            let exposes = match u32::from(header.AceType) {
                ACCESS_DENIED_ACE_TYPE => false,
                ACCESS_ALLOWED_ACE_TYPE => {
                    // SAFETY: the header's type says this entry is an ACCESS_ALLOWED_ACE.
                    let allowed = unsafe { &*entry.cast::<ACCESS_ALLOWED_ACE>() };
                    grants_read_outside(allowed, trusted_readers)
                }
                _ => true,
            };
            if exposes {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Whether `allowed` lets a reader outside `trusted_readers` read the file.
fn grants_read_outside(allowed: &ACCESS_ALLOWED_ACE, trusted_readers: &[PSID]) -> bool {
    if allowed.Mask & READ_ACCESS_RIGHTS == 0 {
        return false;
    }
    let reader: PSID = (&raw const allowed.SidStart).cast_mut().cast();
    !trusted_readers
        .iter()
        // SAFETY: both SIDs are valid for the duration of the comparison.
        .any(|trusted_reader| unsafe { EqualSid(reader, *trusted_reader) } != 0)
}

impl Drop for FileSecurity {
    fn drop(&mut self) {
        // SAFETY: GetNamedSecurityInfoW allocated the descriptor with LocalAlloc.
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

/// The current process's user, read from its access token.
struct CurrentUser {
    /// Word-aligned storage for the `TOKEN_USER` and the SID it points into.
    token_user: Vec<usize>,
}

impl CurrentUser {
    fn query() -> io::Result<Self> {
        let mut token = ptr::null_mut();
        // SAFETY: the process pseudo-handle is valid and `token` is writable handle storage.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcessToken returned a newly owned handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut required = 0;
        // SAFETY: a null buffer and zero size query the required buffer size.
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
        let mut token_user = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
        // SAFETY: the word-aligned buffer holds at least `required` bytes.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                token_user.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { token_user })
    }

    fn sid(&self) -> PSID {
        // SAFETY: a successful TokenUser query put a TOKEN_USER at the buffer's start.
        unsafe { (*self.token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> io::Result<[u32; SID_BUFFER_WORDS]> {
    let mut sid = [0; SID_BUFFER_WORDS];
    let mut size = SECURITY_MAX_SID_SIZE;
    // SAFETY: the buffer holds SECURITY_MAX_SID_SIZE bytes, which fits every well-known SID.
    if unsafe {
        CreateWellKnownSid(
            kind,
            ptr::null_mut(),
            sid.as_mut_ptr().cast(),
            &raw mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(sid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::credentials::{DiscordCredentials, tests::private_credentials_file};

    /// The Users group, which every interactive account belongs to.
    const USERS_GROUP_SID: &str = "*S-1-5-32-545";

    #[test]
    fn rejects_a_file_other_users_can_read() {
        let file = private_credentials_file("bot_token = \"abc.def.ghi\"\n");
        let granted = std::process::Command::new("icacls")
            .arg(file.path())
            .args(["/grant", &format!("{USERS_GROUP_SID}:R")])
            .output()
            .unwrap();
        assert!(granted.status.success(), "{granted:?}");

        let error = DiscordCredentials::load(file.path()).unwrap_err();

        assert!(matches!(error, LoadCredentialsError::Exposed));
    }
}
