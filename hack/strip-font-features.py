#!/usr/bin/env python3
"""Remove contextual alternates and ligatures from the bundled fonts.

Inter turns an "x" between digits into a multiplication sign and "->" into an
arrow; JetBrains Mono joins "--", "->", "!=" and friends into ligatures. A
cluster browser must show names, YAML and logs exactly as they are, and the
UI toolkit has no switch for OpenType features, so the features are removed
from the font files themselves (permitted by the SIL Open Font License).

Usage: hack/strip-font-features.py <font> [<font> ...]   (rewrites in place)
Needs: pip install fonttools
"""
import sys

from fontTools.ttLib import TTFont

DROP = {"calt", "liga", "dlig", "clig", "rlig"}

for path in sys.argv[1:]:
    font = TTFont(path)
    gsub = font["GSUB"].table if "GSUB" in font else None
    removed = []
    if gsub is not None and gsub.FeatureList is not None:
        records = gsub.FeatureList.FeatureRecord
        keep = [i for i, r in enumerate(records) if r.FeatureTag not in DROP]
        removed = sorted({r.FeatureTag for r in records if r.FeatureTag in DROP})
        remap = {old: new for new, old in enumerate(keep)}
        gsub.FeatureList.FeatureRecord = [records[i] for i in keep]
        gsub.FeatureList.FeatureCount = len(keep)
        for script in gsub.ScriptList.ScriptRecord:
            systems = [script.Script.DefaultLangSys] + [l.LangSys for l in script.Script.LangSysRecord]
            for lang in filter(None, systems):
                lang.FeatureIndex = [remap[i] for i in lang.FeatureIndex if i in remap]
                lang.FeatureCount = len(lang.FeatureIndex)
                if lang.ReqFeatureIndex != 0xFFFF:
                    lang.ReqFeatureIndex = remap.get(lang.ReqFeatureIndex, 0xFFFF)
    font.save(path)
    print(f"{path}: removed {', '.join(removed) or 'nothing'}")
