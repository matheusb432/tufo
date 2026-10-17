"""Renders the preset backgrounds the GUI embeds from full-resolution originals.

Each original becomes two lossy WebPs in sRGB, stripped of metadata: a large rendition for the
open collection's page and a small one for cards and the background gallery.

    python3 render.py <originals folder> <backgrounds folder> [key...]
"""
import io
import pathlib
import sys

from PIL import Image, ImageCms, ImageStat

# Some museum scans exceed Pillow's decompression-bomb guard; these files are trusted.
Image.MAX_IMAGE_PIXELS = None

RENDITIONS = {
    # folder: (longest side in pixels, WebP quality)
    "large": (2560, 78),
    "small": (800, 82),
}
BLACK_BORDER_LUMINANCE_MAX = 12
BORDER_SCAN_FRACTION_MAX = 0.12
EDGE_MARGIN_FRACTION = 0.006
# Extra crops, as fractions (left, top, right, bottom) of the border-trimmed painting.
CROP_FRACTIONS_BY_KEY = {}
SRGB_PROFILE = ImageCms.createProfile("sRGB")


def to_srgb(image):
    icc_bytes = image.info.get("icc_profile")
    rgb = image.convert("RGB")
    if not icc_bytes:
        return rgb
    source_profile = ImageCms.ImageCmsProfile(io.BytesIO(icc_bytes))
    if "sRGB" in ImageCms.getProfileDescription(source_profile):
        return rgb
    return ImageCms.profileToProfile(
        rgb, source_profile, SRGB_PROFILE, renderingIntent=ImageCms.Intent.PERCEPTUAL, outputMode="RGB"
    )


def border_depth(gray, side):
    width, height = gray.size
    extent = width if side in ("left", "right") else height
    depth = 0
    while depth < int(extent * BORDER_SCAN_FRACTION_MAX):
        box = {
            "left": (depth, 0, depth + 1, height),
            "right": (width - 1 - depth, 0, width - depth, height),
            "top": (0, depth, width, depth + 1),
            "bottom": (0, height - 1 - depth, width, height - depth),
        }[side]
        if ImageStat.Stat(gray.crop(box)).mean[0] > BLACK_BORDER_LUMINANCE_MAX:
            break
        depth += 1
    return depth


def trim(image, key):
    gray = image.convert("L")
    depths = {side: border_depth(gray, side) for side in ("left", "right", "top", "bottom")}
    width, height = image.size
    margin_x, margin_y = int(width * EDGE_MARGIN_FRACTION), int(height * EDGE_MARGIN_FRACTION)
    image = image.crop((
        depths["left"] + margin_x,
        depths["top"] + margin_y,
        width - depths["right"] - margin_x,
        height - depths["bottom"] - margin_y,
    ))
    if key in CROP_FRACTIONS_BY_KEY:
        left, top, right, bottom = CROP_FRACTIONS_BY_KEY[key]
        width, height = image.size
        image = image.crop((int(left * width), int(top * height), int(right * width), int(bottom * height)))
    return image


def render(original, output):
    key = original.stem
    image = trim(to_srgb(Image.open(original)), key)
    for folder, (side_max, quality) in RENDITIONS.items():
        scale = min(1.0, side_max / max(image.size))
        size = (round(image.width * scale), round(image.height * scale))
        fitted = image.resize(size, Image.Resampling.LANCZOS) if scale < 1.0 else image
        path = output / folder / f"{key}.webp"
        path.parent.mkdir(parents=True, exist_ok=True)
        fitted.save(path, "WEBP", quality=quality, method=6)
        print(f"{key}: {folder} {fitted.size[0]}x{fitted.size[1]} {path.stat().st_size // 1024} KB"
              + ("" if scale < 1.0 else f" (original only {image.size[0]}x{image.size[1]})"))


if __name__ == "__main__":
    originals, output = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    for original in sorted(originals.iterdir()):
        if original.suffix.lower() in (".jpg", ".jpeg", ".png", ".tif", ".tiff") and (
            len(sys.argv) < 4 or original.stem in sys.argv[3:]
        ):
            render(original, output)
