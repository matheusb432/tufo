//! The menu bar's icon, drawn from `tufo-tray.svg`.

use tray_icon::Icon;

use super::draw_icon;

/// The menu bar draws its icons 18 points tall, which is 36 pixels on a Retina display.
const MENU_BAR_ICON_SIDE_PX: u32 = 36;

pub(super) fn icon() -> anyhow::Result<Icon> {
    let rgba = draw_icon(MENU_BAR_ICON_SIDE_PX)?
        .into_iter()
        .flat_map(|color| [color.red(), color.green(), color.blue(), color.alpha()])
        .collect();
    Ok(Icon::from_rgba(
        rgba,
        MENU_BAR_ICON_SIDE_PX,
        MENU_BAR_ICON_SIDE_PX,
    )?)
}
