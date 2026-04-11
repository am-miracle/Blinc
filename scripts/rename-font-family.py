#!/usr/bin/env python3
"""Rewrite the `name` table of a TTF so the font reports a custom family
name to `fontdb` / HarfBuzz consumers.

Why: `blinc_noto_symbols` bundles subsets of Google's Noto Sans and
Noto Sans Math. If those subsets are registered under their original
family names ("Noto Sans" / "Noto Sans Math"), `blinc_text`'s generic
sans-serif fallback chain in `FontRegistry::find_generic_font_id`
finds "Noto Sans" and picks it as the *primary* sans-serif font.
Since the subset only carries the ~150 non-ASCII codepoints the
scanner harvested, every Latin character in the UI then renders as
`.notdef`.

The fix is to rename the subsets to a name that is NOT in the
generic fallback chain — "Blinc Noto Symbols Sans" / "Blinc Noto
Symbols Math" — and add those names explicitly to the emoji /
symbol fallback chains inside `blinc_text`. That way the subsets
are only used when glyphs are actually missing from whatever the
user's primary font is, never as the primary font themselves.

`hb-subset` has no flag for renaming the `name` table, so we do it
with fontTools as a post-processing pass.

Usage:
    python3 scripts/rename-font-family.py <in.ttf> <out.ttf> <new family>

The script rewrites every relevant name-ID entry (1, 4, 6, 16, 17)
for every language/platform record already present, so downstream
consumers see the new name regardless of which record they consult.
"""

import sys
from fontTools.ttLib import TTFont


def rewrite_name_table(in_path: str, out_path: str, new_family: str) -> None:
    font = TTFont(in_path)
    name = font["name"]

    # PostScript names must not contain spaces; collapse them.
    ps_name = new_family.replace(" ", "")
    # Full name = family name for a Regular face.
    full_name = new_family
    # Subfamily stays as-is (usually "Regular" / "Math Regular").

    for record in list(name.names):
        nid = record.nameID
        if nid == 1:  # Font Family Name
            record.string = new_family
        elif nid == 4:  # Full Font Name
            record.string = full_name
        elif nid == 6:  # PostScript Name
            record.string = ps_name
        elif nid == 16:  # Typographic / Preferred Family Name
            record.string = new_family
        # nameIDs 2 (subfamily), 17 (preferred subfamily), 3 (unique
        # identifier) are left untouched — we only need the family
        # identity to change.

    font.save(out_path)


def main() -> int:
    if len(sys.argv) != 4:
        sys.stderr.write(
            "usage: rename-font-family.py <in.ttf> <out.ttf> <new family>\n"
        )
        return 2
    in_path, out_path, new_family = sys.argv[1:4]
    rewrite_name_table(in_path, out_path, new_family)
    return 0


if __name__ == "__main__":
    sys.exit(main())
