"""生成三档 Web 字体，源 OTF 留给画布使用。"""
import hashlib
import json
from pathlib import Path
import sys

import brotli
import fontTools
from fontTools import subset
from fontTools.ttLib import TTFont

if fontTools.__version__ != "4.62.1" or brotli.__version__ != "1.0.9":
    raise RuntimeError("请安装 scripts/font-requirements.txt 中的固定版本")

source, output, corpus = map(Path, sys.argv[1:4])
text = corpus.read_text(encoding="utf-8")
# GB2312 的 6763 个常用简体字；诊断内容超出此集合时由系统字体补字。
common = set()
for high in range(0xB0, 0xF8):
    for low in range(0xA1, 0xFF):
        try:
            common.update(bytes([high, low]).decode("gb2312"))
        except UnicodeDecodeError:
            pass
assert len(common) == 6763
codepoints = {ord(c) for c in text} | {ord(c) for c in common}
for start, end in [(0x20, 0xFF), (0x2000, 0x206F), (0x3000, 0x303F), (0xFF00, 0xFFEF)]:
    codepoints.update(range(start, end + 1))

output.mkdir(parents=True, exist_ok=True)
faces = []
coverage = None
for style in ["Regular", "Medium", "Bold"]:
    original = source / f"SourceHanSansCN-{style}.otf"
    font = TTFont(original, recalcTimestamp=False)
    cmap = font.getBestCmap()
    supported = codepoints & cmap.keys()
    # 所有脸使用相同的字符集合；禁止静默漏掉界面汉字。
    missing_ui = {ord(c) for c in text if '\u3400' <= c <= '\u9fff'} - cmap.keys()
    if missing_ui:
        raise RuntimeError(f"原字体不含界面用字: {sorted(missing_ui)}")
    if coverage is None:
        coverage = sorted(supported)
    assert coverage == sorted(supported)
    options = subset.Options()
    options.recalc_timestamp = False
    options.layout_features = ["*"]
    options.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14, 16, 17]
    options.name_legacy = True
    options.name_languages = ["*"]
    worker = subset.Subsetter(options=options)
    worker.populate(unicodes=supported)
    worker.subset(font)
    # OFL 保留名称不用于修改版。字形不改，内部名称和发行文件使用 CST UI Sans。
    names = {1: "CST UI Sans", 2: style, 3: f"CSTUISans-{style}-1", 4: f"CST UI Sans {style}",
             6: f"CSTUISans-{style}", 16: "CST UI Sans", 17: style}
    for entry in font["name"].names:
        if entry.nameID in names:
            entry.string = names[entry.nameID].encode(entry.getEncoding())
    if "CFF " in font:
        cff = font["CFF "].cff
        cff.fontNames = [f"CSTUISans-{style}"]
        top = cff.topDictIndex[0]
        top.FamilyName = "CST UI Sans"
        top.FullName = f"CST UI Sans {style}"
    font.flavor = "woff2"
    target = output / f"CSTUISans-{style}.woff2"
    font.save(target)
    faces.append({"file": target.name, "sourceBytes": original.stat().st_size,
                  "bytes": target.stat().st_size, "sha256": hashlib.sha256(target.read_bytes()).hexdigest()})
    font.close()

ranges = []
for cp in coverage:
    if ranges and cp == ranges[-1][1] + 1:
        ranges[-1][1] = cp
    else:
        ranges.append([cp, cp])
manifest = {"family": "CST UI Sans", "fontTools": fontTools.__version__, "brotli": brotli.__version__,
            "corpusSha256": hashlib.sha256(corpus.read_bytes()).hexdigest(), "glyphCodepoints": len(coverage),
            "ranges": ranges, "faces": faces}
(output / "subset.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(json.dumps({"characters": len(coverage), "sourceBytes": sum(f["sourceBytes"] for f in faces),
                  "woff2Bytes": sum(f["bytes"] for f in faces)}))
