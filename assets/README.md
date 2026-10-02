# Tufo assets

Tufo tracks its bundled paintings and ambience as ordinary Git files. A clone includes
the media bytes; Git LFS and separate repositories are unnecessary.
The GUI embeds `backgrounds/small/`; the server embeds `effects/`.
The `large/` renditions are retained for artwork maintenance.

| Path | Contents |
| --- | --- |
| `backgrounds/small/`, `backgrounds/large/` | 800 px and up to 2560 px WebP renditions |
| [backgrounds/CREDITS.md](backgrounds/CREDITS.md) | Painting titles, artists, sources, and licenses |
| [backgrounds/SOURCES.md](backgrounds/SOURCES.md) | Download links for full-resolution originals, which are not tracked |
| [backgrounds/render.py](backgrounds/render.py) | Rendition generator |
| [effects/CREDITS.md](effects/CREDITS.md) | Recording authors, source links, edits, and licenses |

The initial snapshot comes from asset revision `429b193208b84e7e7abeec452a460bc78011a92f`.
Its 40 WebP renditions and eight Ogg files preserve the original LFS object SHA-256 values.
The effects originate from Blanket's pinned revision recorded in their credits.

## Updating assets

Download a painting's original outside this repository and record its source and license.
Render it with `uv run --no-project --with pillow assets/backgrounds/render.py <originals-folder> assets/backgrounds <key>`.
Update its preset, presentation credit, and database constraint when adding a new key.

For effects, use recordings whose redistribution terms permit inclusion, preserve attribution,
and check seamless looping. Update `DefaultEffect`, `default_effects.rs`, and the credits.
Changes to bundled defaults affect newly seeded libraries.
Run `just check` and `just test` before committing; the image and effect tests decode the shipped media.
