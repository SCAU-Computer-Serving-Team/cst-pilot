#!/usr/bin/env python3
"""Pure-Python ROI visual pixel analyzer for Desktop UX E2E verification (`doc/desktop/test/ux-e2e.md`).

Decodes PNG screenshots using Python standard library only (zlib + struct) so it runs reliably
on any Windows machine without third-party imaging packages.
"""

from __future__ import annotations

import argparse
import json
import struct
import zlib
from pathlib import Path
from typing import Any

MAX_SCREENSHOT_PIXELS = 32 * 1024 * 1024


def _paeth(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa = abs(p - a)
    pb = abs(p - b)
    pc = abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def decode_png_rgb(path: Path) -> tuple[int, int, bytearray]:
    """Decode an 8-bit non-interlaced PNG (color types 2=RGB, 6=RGBA, 0=Gray) into packed RGB bytes."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"Not a PNG file: {path}")

    pos = 8
    width = height = bit_depth = color_type = interlace = 0
    idat_chunks: list[bytes] = []
    seen_end = False
    while pos + 12 <= len(data):
        length, ctype = struct.unpack_from(">I4s", data, pos)
        if pos + 12 + length > len(data):
            raise ValueError(f"Truncated PNG chunk: {path}")
        chunk = data[pos + 8 : pos + 8 + length]
        if pos == 8 and ctype != b"IHDR":
            raise ValueError(f"Missing PNG header: {path}")
        if ctype == b"IHDR":
            if pos != 8 or length != 13:
                raise ValueError(f"Invalid PNG header: {path}")
            width, height, bit_depth, color_type, comp, filt, interlace = struct.unpack(
                ">IIBBBBB", chunk
            )
            if not width or not height or width * height > MAX_SCREENSHOT_PIXELS:
                raise ValueError(f"Unsupported PNG dimensions {width}x{height}: {path}")
            if comp != 0 or filt != 0:
                raise ValueError(f"Unsupported PNG compression or filter method: {path}")
        elif ctype == b"IDAT":
            idat_chunks.append(chunk)
        elif ctype == b"IEND" and not length:
            seen_end = True
            break
        pos += 12 + length

    if not seen_end or not idat_chunks:
        raise ValueError(f"Incomplete PNG: {path}")
    if bit_depth != 8 or interlace != 0:
        raise ValueError(f"Unsupported PNG format (depth={bit_depth}, interlace={interlace}): {path}")
    bpp_map = {0: 1, 2: 3, 6: 4}
    if color_type not in bpp_map:
        raise ValueError(f"Unsupported PNG color_type={color_type}: {path}")
    bpp = bpp_map[color_type]
    stride = width * bpp

    raw_len = (stride + 1) * height
    decoder = zlib.decompressobj()
    raw = decoder.decompress(b"".join(idat_chunks), raw_len + 1)
    if len(raw) != raw_len or not decoder.eof or decoder.unused_data:
        raise ValueError(f"Invalid PNG scanline data: {path}")
    rgb = bytearray(width * height * 3)
    prev_row = bytearray(stride)
    cur_row = bytearray(stride)
    in_idx = 0

    for y in range(height):
        filt = raw[in_idx]
        in_idx += 1
        row_slice = raw[in_idx : in_idx + stride]
        in_idx += stride

        if filt == 0:
            cur_row[:] = row_slice
        elif filt == 1:
            for i in range(stride):
                left = cur_row[i - bpp] if i >= bpp else 0
                cur_row[i] = (row_slice[i] + left) & 0xFF
        elif filt == 2:
            for i in range(stride):
                cur_row[i] = (row_slice[i] + prev_row[i]) & 0xFF
        elif filt == 3:
            for i in range(stride):
                left = cur_row[i - bpp] if i >= bpp else 0
                cur_row[i] = (row_slice[i] + ((left + prev_row[i]) >> 1)) & 0xFF
        elif filt == 4:
            for i in range(stride):
                left = cur_row[i - bpp] if i >= bpp else 0
                up = prev_row[i]
                up_left = prev_row[i - bpp] if i >= bpp else 0
                cur_row[i] = (row_slice[i] + _paeth(left, up, up_left)) & 0xFF
        else:
            raise ValueError(f"Unknown PNG filter {filt} at row {y}")

        out_base = y * width * 3
        if bpp == 3:
            rgb[out_base : out_base + width * 3] = cur_row
        elif bpp == 4:
            for x in range(width):
                s = x * 4
                d = out_base + x * 3
                rgb[d] = cur_row[s]
                rgb[d + 1] = cur_row[s + 1]
                rgb[d + 2] = cur_row[s + 2]
        else:
            for x in range(width):
                v = cur_row[x]
                d = out_base + x * 3
                rgb[d] = v
                rgb[d + 1] = v
                rgb[d + 2] = v

        prev_row, cur_row = cur_row, prev_row

    return width, height, rgb


def _clamp_roi(w: int, h: int, roi: list[int] | None) -> tuple[int, int, int, int]:
    if roi is None:
        return 0, 0, w, h
    x, y, rw, rh = roi
    if any(type(v) is not int for v in roi) or rw <= 0 or rh <= 0:
        raise ValueError("ROI requires integer [x, y, width, height] with positive dimensions")
    x0, y0 = max(0, x), max(0, y)
    x1, y1 = min(w, x + rw), min(h, y + rh)
    if x0 >= x1 or y0 >= y1:
        raise ValueError("ROI does not intersect the image")
    return x0, y0, x1, y1


def _srgb_to_linear(c: float) -> float:
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def _rel_luminance(r: int, g: int, b: int) -> float:
    return (
        0.2126 * _srgb_to_linear(r / 255.0)
        + 0.7152 * _srgb_to_linear(g / 255.0)
        + 0.0722 * _srgb_to_linear(b / 255.0)
    )


def analyze_freeze_diff(img_a: Path, img_b: Path, roi: list[int] | None = None) -> dict[str, Any]:
    w1, h1, rgb1 = decode_png_rgb(img_a)
    w2, h2, rgb2 = decode_png_rgb(img_b)
    if (w1, h1) != (w2, h2):
        return {
            "pass": False,
            "reason": f"size_mismatch ({w1}x{h1} vs {w2}x{h2})",
            "meanL1": None,
            "maxDiff": None,
            "changedPixels": None,
        }
    x0, y0, x1, y1 = _clamp_roi(w1, h1, roi)
    total_abs = 0
    max_diff = 0
    changed_pixels = 0
    count = (x1 - x0) * (y1 - y0)
    for y in range(y0, y1):
        row_off = y * w1 * 3
        for x in range(x0, x1):
            idx = row_off + x * 3
            dr = abs(rgb1[idx] - rgb2[idx])
            dg = abs(rgb1[idx + 1] - rgb2[idx + 1])
            db = abs(rgb1[idx + 2] - rgb2[idx + 2])
            s = dr + dg + db
            total_abs += s
            if s > 0:
                changed_pixels += 1
            m = max(dr, dg, db)
            if m > max_diff:
                max_diff = m
    mean_l1 = total_abs / (count * 3)
    return {
        "pass": max_diff == 0,
        "roi": [x0, y0, x1 - x0, y1 - y0],
        "meanL1": round(mean_l1, 6),
        "maxDiff": max_diff,
        "changedPixels": changed_pixels,
        "totalPixels": count,
    }


def analyze_contrast(img_path: Path, roi: list[int] | None = None, min_ratio: float = 4.5) -> dict[str, Any]:
    w, h, rgb = decode_png_rgb(img_path)
    x0, y0, x1, y1 = _clamp_roi(w, h, roi)
    lums: list[float] = []
    # Subsample slightly on huge ROIs while keeping exact accuracy for text strokes
    step = 1 if (x1 - x0) * (y1 - y0) <= 200_000 else 2
    for y in range(y0, y1, step):
        row_off = y * w * 3
        for x in range(x0, x1, step):
            idx = row_off + x * 3
            lums.append(_rel_luminance(rgb[idx], rgb[idx + 1], rgb[idx + 2]))
    lums.sort()
    n = len(lums)
    if n < 10:
        return {"pass": False, "reason": "roi_too_small", "ratio": 0.0}
    bg_lum = lums[n // 2]
    # Compare foreground extremes on whichever side deviates most from the dominant background
    low_fg = lums[max(0, int(n * 0.01))]
    high_fg = lums[min(n - 1, int(n * 0.99))]
    r_low = (max(bg_lum, low_fg) + 0.05) / (min(bg_lum, low_fg) + 0.05)
    r_high = (max(bg_lum, high_fg) + 0.05) / (min(bg_lum, high_fg) + 0.05)
    ratio = max(r_low, r_high)
    return {
        "pass": ratio >= min_ratio,
        "roi": [x0, y0, x1 - x0, y1 - y0],
        "bgLuminance": round(bg_lum, 4),
        "fgLuminance": round(low_fg if r_low >= r_high else high_fg, 4),
        "ratio": round(ratio, 2),
        "minRequired": min_ratio,
    }


def analyze_surface_elevation(img_path: Path, box: list[int], is_dark: bool = False) -> dict[str, Any]:
    """Inspect an elevated card / frosted overlay box [x, y, w, h] for L2 outer shadow decay and L4 top edge."""
    w, h, rgb = decode_png_rgb(img_path)
    x0, y0, x1, y1 = _clamp_roi(w, h, box)

    inset = max(12, (x1 - x0) // 6)
    mid_x0, mid_x1 = x0 + inset, x1 - inset
    if mid_x1 - mid_x0 < 2 or y1 - y0 <= 10:
        return {"shadowDetected": False, "reason": "box_too_small"}

    def row_mean_luma(y: int, rx0: int, rx1: int) -> float:
        row_off = y * w * 3
        s = 0.0
        for x in range(rx0, rx1):
            idx = row_off + x * 3
            s += 0.299 * rgb[idx] + 0.587 * rgb[idx + 1] + 0.114 * rgb[idx + 2]
        return s / (rx1 - rx0)

    # Outer bottom shadow decay profile using only in-bounds offsets below the card
    shadow_profile: dict[str, float] = {}
    valid_samples: list[tuple[int, float]] = []
    for dy in (2, 6, 12, 16, 24):
        if y1 + dy < h:
            val = round(row_mean_luma(y1 + dy, mid_x0, mid_x1), 2)
            shadow_profile[f"dy_{dy}px"] = val
            valid_samples.append((dy, val))

    # Top edge highlight / border vs inner body
    top_edge_luma = round(row_mean_luma(y0 + 1, mid_x0, mid_x1), 2)
    inner_body_luma = round(row_mean_luma(y0 + 10, mid_x0, mid_x1), 2)

    # Shadow decay exists if immediate outer ring is darker than the farthest in-bounds backdrop row
    if len(valid_samples) >= 2:
        shadow_near = valid_samples[0][1]
        shadow_far = valid_samples[-1][1]
        shadow_drop = round(shadow_far - shadow_near, 2)
    else:
        shadow_drop = 0.0

    # Horizontal gradient smoothness inside the top portion of the card (detects smooth blur vs harsh noise)
    grad_sum = 0.0
    grad_cnt = 0
    gy0 = y0 + 6
    gy1 = min(y1, y0 + min(48, max(8, (y1 - y0) // 3)))
    for y in range(gy0, gy1):
        row_off = y * w * 3
        for x in range(mid_x0, mid_x1 - 1):
            i0 = row_off + x * 3
            i1 = i0 + 3
            grad_sum += (
                abs(rgb[i1] - rgb[i0])
                + abs(rgb[i1 + 1] - rgb[i0 + 1])
                + abs(rgb[i1 + 2] - rgb[i0 + 2])
            ) / 3.0
            grad_cnt += 1
    mean_grad = round(grad_sum / grad_cnt, 3)

    return {
        "box": [x0, y0, x1 - x0, y1 - y0],
        "isDark": is_dark,
        "topEdgeLuma": top_edge_luma,
        "innerBodyLuma": inner_body_luma,
        "topHighlightDelta": round(top_edge_luma - inner_body_luma, 2),
        "shadowProfile": shadow_profile,
        "shadowOuterDarkening": shadow_drop,
        "shadowDetected": shadow_drop >= (0.4 if is_dark else 0.8),
        "surfaceMeanGradient": mean_grad,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Desktop UX E2E visual pixel analyzer")
    parser.add_argument("--manifest", required=True, help="Input JSON manifest of visual checks")
    parser.add_argument("--output", required=True, help="Output JSON report path")
    args = parser.parse_args()

    manifest = json.loads(Path(args.manifest).read_text(encoding="utf-8"))
    results: dict[str, Any] = {"checks": {}, "allPassed": True}

    for item in manifest.get("checks", []):
        name = item["name"]
        kind = item["kind"]
        try:
            if kind == "freeze_diff":
                res = analyze_freeze_diff(Path(item["imageA"]), Path(item["imageB"]), item.get("roi"))
            elif kind == "contrast":
                res = analyze_contrast(
                    Path(item["image"]), item.get("roi"), float(item.get("minRatio", 4.5))
                )
            elif kind == "elevation":
                res = analyze_surface_elevation(
                    Path(item["image"]), item["box"], bool(item.get("isDark", False))
                )
                res["pass"] = res["shadowDetected"]
            else:
                res = {"pass": False, "reason": f"unknown_kind:{kind}"}
        except (ValueError, OSError, zlib.error) as exc:
            res = {"pass": False, "reason": str(exc)}

        results["checks"][name] = res
        if not res.get("pass", False):
            results["allPassed"] = False

    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({"output": str(out_path), "allPassed": results["allPassed"]}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
