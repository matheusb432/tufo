//! Renders Tufo's icon sources in `crates/tufo-gui/icons` into the Windows icons the executable
//! embeds: `tufo.ico` for Explorer, the Start menu, and the taskbar, and `tufo-tray.ico` for the
//! notification area. The Linux install renders its fixed-size PNGs from the same sources.

use std::path::Path;

use anyhow::{Context as _, Result};
use resvg::{tiny_skia, usvg};

pub(crate) const ICONS_DIR: &str = "crates/tufo-gui/icons";
/// The art for 16 to 32 px, with a rim and features thick enough to survive those sizes.
pub(crate) const SMALL_ICON_SVG: &str = "tufo-small.svg";
/// Entries this large are stored as PNG; smaller ones as bitmaps, which every Windows API reads.
const PNG_ENTRY_SIDE_PX_MIN: u32 = 256;

/// One icon file, and which source draws each of its sizes.
struct IcoPlan {
    file_name: &'static str,
    sources: &'static [(&'static str, &'static [u32])],
}

/// Windows asks for 16 to 32 px at 100% to 200% scaling, and Explorer's large views for 256 px.
const APP_ICO: IcoPlan = IcoPlan {
    file_name: "tufo.ico",
    sources: &[
        (SMALL_ICON_SVG, &[16, 20, 24, 32]),
        ("tufo.svg", &[48, 64, 256]),
    ],
};
/// The notification area loads the size matching the system's small-icon metric.
const TRAY_ICO: IcoPlan = IcoPlan {
    file_name: "tufo-tray.ico",
    sources: &[("tufo-tray.svg", &[16, 20, 24, 32, 40, 48, 64])],
};

pub(crate) fn render(workspace_root: &Path) -> Result<()> {
    let icons = workspace_root.join(ICONS_DIR);
    for plan in [APP_ICO, TRAY_ICO] {
        let ico_path = icons.join(plan.file_name);
        std::fs::write(&ico_path, ico_from_plan(&icons, &plan)?)
            .with_context(|| format!("write {}", ico_path.display()))?;
    }
    Ok(())
}

/// A PNG of the SVG at `side` × `side` pixels.
pub(crate) fn png(svg: &[u8], side: u32) -> Result<Vec<u8>> {
    rasterize(&parse(svg)?, side)?
        .encode_png()
        .context("encode the PNG")
}

fn parse(svg: &[u8]) -> Result<usvg::Tree> {
    usvg::Tree::from_data(svg, &usvg::Options::default()).context("parse the SVG")
}

fn ico_from_plan(icons: &Path, plan: &IcoPlan) -> Result<Vec<u8>> {
    let mut sides = Vec::new();
    let mut entries = Vec::new();
    for &(svg_name, source_sides) in plan.sources {
        let svg_path = icons.join(svg_name);
        let svg =
            std::fs::read(&svg_path).with_context(|| format!("read {}", svg_path.display()))?;
        let tree = parse(&svg)?;
        for &side in source_sides {
            let pixmap = rasterize(&tree, side)?;
            entries.push(if side >= PNG_ENTRY_SIDE_PX_MIN {
                pixmap.encode_png().context("encode a PNG entry")?
            } else {
                bitmap_entry(&pixmap)
            });
            sides.push(side);
        }
    }
    ico(&sides, &entries)
}

fn rasterize(tree: &usvg::Tree, side: u32) -> Result<tiny_skia::Pixmap> {
    let mut pixmap = tiny_skia::Pixmap::new(side, side).context("allocate the icon")?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "icon sides are far inside f32's exact integer range"
    )]
    let scale = side as f32 / tree.size().width();
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

/// A 32-bit bitmap entry: its header, BGRA rows from the bottom up, and an all-clear AND mask,
/// since the alpha channel already carries the shape.
fn bitmap_entry(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    const HEADER_BYTES: u32 = 40;
    let side = pixmap.width();
    let mask_row_bytes = side.div_ceil(32) * 4;
    let mut entry = Vec::new();
    entry.extend(HEADER_BYTES.to_le_bytes());
    entry.extend(side.to_le_bytes());
    // An icon's bitmap height counts the color rows and the mask rows.
    entry.extend((side * 2).to_le_bytes());
    entry.extend(1_u16.to_le_bytes());
    entry.extend(32_u16.to_le_bytes());
    entry.extend([0; 24]);
    for row in pixmap.pixels().chunks(side as usize).rev() {
        for pixel in row {
            let color = pixel.demultiply();
            entry.extend([color.blue(), color.green(), color.red(), color.alpha()]);
        }
    }
    entry.resize(entry.len() + (mask_row_bytes * side) as usize, 0);
    entry
}

/// The ICO container: a header, one directory entry per image, then the images.
fn ico(sides: &[u32], entries: &[Vec<u8>]) -> Result<Vec<u8>> {
    const HEADER_BYTES: usize = 6;
    const DIRECTORY_ENTRY_BYTES: usize = 16;
    let mut ico = Vec::new();
    ico.extend(0_u16.to_le_bytes());
    ico.extend(1_u16.to_le_bytes());
    ico.extend(u16::try_from(entries.len())?.to_le_bytes());
    let mut offset = HEADER_BYTES + DIRECTORY_ENTRY_BYTES * entries.len();
    for (&side, entry) in sides.iter().zip(entries) {
        // A side of 256 is written as 0.
        let side = u8::try_from(side).unwrap_or(0);
        ico.extend([side, side, 0, 0]);
        ico.extend(1_u16.to_le_bytes());
        ico.extend(32_u16.to_le_bytes());
        ico.extend(u32::try_from(entry.len())?.to_le_bytes());
        ico.extend(u32::try_from(offset)?.to_le_bytes());
        offset += entry.len();
    }
    for entry in entries {
        ico.extend(entry);
    }
    Ok(ico)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icons() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(ICONS_DIR)
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn u32_at(bytes: &[u8], offset: usize) -> usize {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
    }

    #[test]
    fn each_icon_holds_every_planned_size_where_its_directory_says() {
        for plan in [APP_ICO, TRAY_ICO] {
            let ico = ico_from_plan(&icons(), &plan).unwrap();
            let sides = plan
                .sources
                .iter()
                .flat_map(|(_, sides)| sides.iter().copied())
                .collect::<Vec<_>>();

            assert_eq!(u16_at(&ico, 2), 1);
            assert_eq!(usize::from(u16_at(&ico, 4)), sides.len());
            for (index, side) in sides.into_iter().enumerate() {
                assert_entry(&ico, index, side);
            }
        }
    }

    /// Checks one directory entry and the image it points at.
    fn assert_entry(ico: &[u8], index: usize, side: u32) {
        let directory = 6 + 16 * index;
        let (length, offset) = (u32_at(ico, directory + 8), u32_at(ico, directory + 12));
        let entry = &ico[offset..offset + length];
        if side >= PNG_ENTRY_SIDE_PX_MIN {
            assert_eq!(ico[directory], 0);
            assert_eq!(&entry[1..4], b"PNG");
            return;
        }
        assert_eq!(u32::from(ico[directory]), side);
        let side = side as usize;
        assert_eq!(length, 40 + side * side * 4 + side.div_ceil(32) * 4 * side);
    }

    #[test]
    fn bitmap_rows_run_from_the_bottom_up_in_bgra() {
        let mut pixmap = tiny_skia::Pixmap::new(2, 2).unwrap();
        pixmap.pixels_mut()[0] = tiny_skia::ColorU8::from_rgba(10, 20, 30, 255).premultiply();

        let entry = bitmap_entry(&pixmap);

        let top_left = 40 + 2 * 4;
        assert_eq!(&entry[top_left..top_left + 4], &[30, 20, 10, 255]);
    }
}
