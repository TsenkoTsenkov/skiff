#!/usr/bin/env python3
"""Renders the demo animation from the frames recorded by the `demo` test.

  python3 hack/demo/render.py [--frames DIR] [--out DIR] [--sheet FILE]

Reads DIR/manifest.jsonl and the numbered PNGs next to it (written by
`cargo test -p skiff --test demo -- --ignored`, default target/demo/frames)
and writes skiff-demo.gif and skiff-demo.mp4 to --out (default target/demo).

The pictures of the app are used as they were drawn. This script adds only
what a screen recording would have had around them: a caption bar under
the window, the mouse pointer (the recording knows where it was), a ring
where it clicks, and a cross-fade from one scene to the next.

Needs Pillow, and ffmpeg on PATH.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
FONTS = os.path.join(REPO, "crates", "skiff", "assets", "fonts")
ICON = os.path.join(REPO, "crates", "skiff", "assets", "icons", "skiff-64.png")

BAR = 64  # height of the caption bar
BAR_BG = (13, 17, 23)
BAR_EDGE = (38, 45, 56)
TEXT = (230, 237, 243)
DIM = (125, 133, 144)
ACCENT = (88, 166, 255)


def load(frames_dir):
    """The manifest as a list of shots, each knowing its scene."""
    shots, captions, size = [], [], None
    with open(os.path.join(frames_dir, "manifest.jsonl"), encoding="utf-8") as f:
        for line in f:
            entry = json.loads(line)
            if "size" in entry:
                size = tuple(int(n) for n in entry["size"])
            elif "scene" in entry:
                captions.append(entry["scene"])
            else:
                entry["scene"] = len(captions) - 1
                entry["image"] = Image.open(os.path.join(frames_dir, entry["file"])).convert("RGB")
                shots.append(entry)
    if not shots or size is None:
        sys.exit(f"no frames in {frames_dir}: record them first (see hack/demo/README.md)")
    return shots, captions, size


def pointer_sprite():
    """An arrow pointer with a soft shadow, drawn large and scaled down."""
    k = 8
    outline = [(0, 0), (0, 17), (4, 13.2), (7, 19.6), (9.6, 18.4), (6.7, 12.2), (12, 12.2)]
    w, h = 22, 28
    arrow = Image.new("RGBA", (w * k, h * k), (0, 0, 0, 0))
    draw = ImageDraw.Draw(arrow)
    points = [((x + 3) * k, (y + 3) * k) for x, y in outline]
    draw.polygon(points, fill=(255, 255, 255, 255))
    draw.line(points + [points[0]], fill=(20, 20, 20, 255), width=int(1.3 * k), joint="curve")
    shadow = Image.new("RGBA", arrow.size, (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 110), (k, int(1.5 * k)), arrow.getchannel("A"))
    shadow = shadow.filter(ImageFilter.GaussianBlur(1.2 * k))
    sprite = Image.alpha_composite(shadow, arrow).resize((w, h), Image.LANCZOS)
    return sprite, (3, 3)  # the image and where its tip is


class Composer:
    def __init__(self, size, captions):
        self.size = size
        self.captions = captions
        self.font = ImageFont.truetype(os.path.join(FONTS, "Inter-SemiBold.otf"), 22)
        self.small = ImageFont.truetype(os.path.join(FONTS, "Inter-Regular.otf"), 15)
        self.sprite, self.tip = pointer_sprite()
        self.icon = Image.open(ICON).convert("RGBA").resize((30, 30), Image.LANCZOS)
        self.bars = [self.bar(i) for i in range(len(captions))]

    def bar(self, scene):
        width = self.size[0]
        bar = Image.new("RGB", (width, BAR), BAR_BG)
        draw = ImageDraw.Draw(bar)
        draw.line([(0, 0), (width, 0)], fill=BAR_EDGE)
        bar.paste(self.icon, (20, (BAR - 30) // 2 + 1), self.icon)
        draw.text((64, BAR // 2 + 1), self.captions[scene], font=self.font, fill=TEXT, anchor="lm")
        # Which scene this is, as dots on the right.
        n = len(self.captions)
        x = width - 24 - (n - 1) * 18
        for i in range(n):
            cx, cy = x + i * 18, BAR // 2 + 1
            r = 4.5 if i == scene else 3.5
            draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=ACCENT if i == scene else (60, 68, 80))
        return bar

    def compose(self, shot, pointer, ring=None):
        """A shot with its caption bar, the pointer at `pointer` and, with
        `ring` (0..1), a click ring that far through its expansion."""
        w, h = self.size
        out = Image.new("RGB", (w, h + BAR))
        out.paste(shot["image"], (0, 0))
        out.paste(self.bars[shot["scene"]], (0, h))
        if pointer is not None:
            x, y = pointer
            if ring is not None:
                k = 4
                radius = 9 + 13 * ring
                box = int((radius + 4) * 2 * k)
                layer = Image.new("RGBA", (box, box), (0, 0, 0, 0))
                alpha = int(235 * (1 - 0.75 * ring))
                ImageDraw.Draw(layer).ellipse(
                    [box / 2 - radius * k, box / 2 - radius * k, box / 2 + radius * k, box / 2 + radius * k],
                    outline=ACCENT + (alpha,),
                    width=int(2.5 * k),
                )
                layer = layer.resize((box // k, box // k), Image.LANCZOS)
                out.paste(layer, (round(x - box / k / 2), round(y - box / k / 2)), layer)
            out.paste(self.sprite, (round(x) - self.tip[0], round(y) - self.tip[1]), self.sprite)
        return out


def ease(t):
    return t * t * (3 - 2 * t)


def timeline(shots, composer, step_ms, fade_ms):
    """The animation as (image, milliseconds) pairs. `step_ms` is the
    length of one step where something moves (pointer, fade, ring)."""
    out = []
    pointer = None
    last = None
    for i, shot in enumerate(shots):
        target = tuple(shot["pointer"]) if shot["pointer"] else None
        start = pointer if pointer is not None else target
        first = composer.compose(shot, start if shot["glide"] else target)
        if last is not None and shot["scene"] != shots[i - 1]["scene"]:
            n = max(1, round(fade_ms / step_ms))
            for k in range(1, n + 1):
                out.append((Image.blend(last, first, ease(k / (n + 1))), step_ms))
        if shot["glide"] and start != target:
            n = max(2, round(shot["ms"] / step_ms))
            for k in range(1, n + 1):
                t = ease(k / n)
                at = (start[0] + (target[0] - start[0]) * t, start[1] + (target[1] - start[1]) * t)
                out.append((composer.compose(shot, at), shot["ms"] / n))
        elif shot["click"]:
            n = max(2, round(shot["ms"] / step_ms))
            for k in range(n):
                out.append((composer.compose(shot, target, ring=k / (n - 1)), shot["ms"] / n))
        else:
            out.append((composer.compose(shot, target), shot["ms"]))
        pointer = target
        last = composer.compose(shot, target)
    # Back to the start, so that the loop does not jump.
    first = composer.compose(shots[0], tuple(shots[0]["pointer"]) if shots[0]["pointer"] else None)
    n = max(1, round(fade_ms / step_ms))
    for k in range(1, n + 1):
        out.append((Image.blend(last, first, ease(k / (n + 1))), step_ms))
    return merge(out)


def merge(frames):
    """Joins neighbours that show the same picture."""
    out = []
    for image, ms in frames:
        if out and ImageChops.difference(out[-1][0], image).getbbox() is None:
            out[-1] = (out[-1][0], out[-1][1] + ms)
        else:
            out.append((image, ms))
    return out


def write_sequence(frames, directory, scale=None):
    """Numbered PNGs and an ffmpeg concat list with each one's duration."""
    lines = ["ffconcat version 1.0"]
    name = None
    for i, (image, ms) in enumerate(frames):
        if scale:
            image = image.resize(scale, Image.LANCZOS)
        name = f"{i:05d}.png"
        image.save(os.path.join(directory, name), compress_level=1)
        lines += [f"file {name}", f"duration {ms / 1000:.3f}"]
    lines.append(f"file {name}")  # the concat demuxer needs the last one named twice
    path = os.path.join(directory, "list.ffconcat")
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    return path


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args], check=True)


def contact_sheet(frames, path, columns=4, thumb=640):
    """Evenly spaced frames of an animation on one sheet, to look it over."""
    total = sum(ms for _, ms in frames)
    count = columns * 6
    picks, t, k = [], 0.0, 0
    for image, ms in frames:
        while k < count and t <= (k + 0.5) * total / count < t + ms:
            picks.append(((k + 0.5) * total / count, image))
            k += 1
        t += ms
    w, h = picks[0][1].size
    th = round(thumb * h / w)
    rows = -(-len(picks) // columns)
    sheet = Image.new("RGB", (columns * thumb, rows * th), (0, 0, 0))
    font = ImageFont.truetype(os.path.join(FONTS, "Inter-SemiBold.otf"), 16)
    for n, (at, image) in enumerate(picks):
        cell = image.resize((thumb, th), Image.LANCZOS)
        draw = ImageDraw.Draw(cell)
        draw.rectangle([0, 0, 64, 22], fill=(0, 0, 0))
        draw.text((5, 2), f"{at / 1000:.1f} s", font=font, fill=(255, 220, 90))
        sheet.paste(cell, ((n % columns) * thumb, (n // columns) * th))
    sheet.save(path)


def gif_frames(path):
    """The frames of a written GIF, as (image, ms), to check the result."""
    frames = []
    with Image.open(path) as gif:
        for i in range(gif.n_frames):
            gif.seek(i)
            frames.append((gif.convert("RGB"), gif.info.get("duration", 100)))
    return frames


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--frames", default=os.path.join(REPO, "target", "demo", "frames"))
    parser.add_argument("--out", default=os.path.join(REPO, "target", "demo"))
    parser.add_argument("--gif-width", type=int, default=0, help="scale the GIF to this width (default: as recorded, which is also the smallest file)")
    parser.add_argument("--gif-colors", type=int, default=256)
    parser.add_argument("--sheet", help="also write a contact sheet of the finished GIF's frames to this file")
    args = parser.parse_args()
    if not shutil.which("ffmpeg"):
        sys.exit("ffmpeg is needed on PATH")

    shots, captions, size = load(args.frames)
    composer = Composer(size, captions)
    os.makedirs(args.out, exist_ok=True)
    gif = os.path.join(args.out, "skiff-demo.gif")
    mp4 = os.path.join(args.out, "skiff-demo.mp4")
    full = (size[0], size[1] + BAR)

    # The MP4: every moving step at 30 per second, full size.
    frames = timeline(shots, composer, step_ms=1000 / 30, fade_ms=330)
    with tempfile.TemporaryDirectory(prefix="skiff-demo-") as tmp:
        listing = write_sequence(frames, tmp)
        ffmpeg(
            "-f", "concat", "-i", listing,
            "-vf", "fps=30,format=yuv420p",
            "-c:v", "libx264", "-preset", "veryslow", "-crf", "24", "-tune", "animation",
            "-movflags", "+faststart", "-an", mp4,
        )  # fmt: skip
    seconds = sum(ms for _, ms in frames) / 1000

    # The GIF: 12 steps a second where something moves, one frame where
    # nothing does, and one palette for the whole of it (so that
    # only what changes between two frames has to be stored).
    frames = timeline(shots, composer, step_ms=1000 / 12, fade_ms=250)
    width = args.gif_width or full[0]
    scale = (width, round(full[1] * width / full[0] / 2) * 2)
    with tempfile.TemporaryDirectory(prefix="skiff-demo-") as tmp:
        listing = write_sequence(frames, tmp, None if scale == full else scale)
        palette = os.path.join(tmp, "palette.png")
        ffmpeg("-f", "concat", "-i", listing, "-vf", f"palettegen=max_colors={args.gif_colors}:stats_mode=full", palette)
        ffmpeg(
            "-f", "concat", "-i", listing, "-i", palette,
            "-lavfi", "paletteuse=dither=none:diff_mode=rectangle",
            "-fps_mode", "vfr", "-loop", "0", gif,
        )  # fmt: skip

    for path in (gif, mp4):
        print(f"{os.path.relpath(path, REPO)}: {os.path.getsize(path) / 1e6:.2f} MB")
    print(f"{seconds:.1f} s, {len(shots)} recorded frames, {len(frames)} GIF frames at {scale[0]}x{scale[1]}, MP4 at {full[0]}x{full[1]}")
    if args.sheet:
        contact_sheet(gif_frames(gif), args.sheet)
        print(f"contact sheet: {args.sheet}")


if __name__ == "__main__":
    main()
