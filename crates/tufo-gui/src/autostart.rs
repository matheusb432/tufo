//! The login entry, which launches the window with [`AT_LOGIN_FLAG`]: hidden in the tray, resuming
//! the saved loops.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::{is_enabled, set_enabled};
#[cfg(windows)]
pub(crate) use windows::{is_enabled, set_enabled};

pub(crate) const AT_LOGIN_FLAG: &str = "--at-login";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Launch {
    ByHand,
    AtLogin,
}

impl Launch {
    pub(crate) fn from_args(mut args: impl Iterator<Item = std::ffi::OsString>) -> Self {
        if args.any(|arg| arg == AT_LOGIN_FLAG) {
            Self::AtLogin
        } else {
            Self::ByHand
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_login_flag_marks_a_login_launch() {
        let launch = |args: &[&str]| Launch::from_args(args.iter().map(Into::into));

        assert_eq!(launch(&["tufo-gui", AT_LOGIN_FLAG]), Launch::AtLogin);
        assert_eq!(launch(&["tufo-gui"]), Launch::ByHand);
        assert_eq!(launch(&["tufo-gui", "--at-login=yes"]), Launch::ByHand);
    }
}
