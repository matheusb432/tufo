//! The login entry, which launches the window with [`AT_LOGIN_FLAG`]: hidden in the tray, resuming
//! the saved loops.

pub(crate) use tufo_client::desktop::autostart::{AT_LOGIN_FLAG, set_enabled};

pub(crate) fn is_enabled() -> bool {
    tufo_client::desktop::autostart::read_enabled().unwrap_or(false)
}

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
