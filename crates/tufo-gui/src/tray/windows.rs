//! The notification area's icon is `tufo-tray.ico`, embedded as resource 2, loaded at the system's
//! small-icon size so Windows picks the matching entry.

use tray_icon::Icon;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

/// `tufo-gui.rc` embeds `tufo-tray.ico` under this ID.
const TRAY_ICON_RESOURCE_ID: u16 = 2;

pub(super) fn icon() -> anyhow::Result<Icon> {
    // SAFETY: GetSystemMetrics has no preconditions.
    let small_icon_side = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let dimensions = u32::try_from(small_icon_side)
        .ok()
        .filter(|&side| side > 0)
        .map(|side| (side, side));
    Ok(Icon::from_resource(TRAY_ICON_RESOURCE_ID, dimensions)?)
}
