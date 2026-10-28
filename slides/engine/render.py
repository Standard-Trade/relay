"""Render a TikTok / Instagram slideshow from a style file and a slides file.

Usage: python3 render.py styles/<style>.json projects/<name>/slides.json

A style says how slides look (canvas, background, text treatment, limits).
A slides file says what they say (text per slide, optional image per slide).
Keeping them apart lets one style render any content, and one script render in
any style.

Writes to projects/<name>/out/<style>/:
  NN.png       one image per slide, at the style's canvas size
  sheet.png    contact sheet at about phone-feed size
  caption.txt  the post caption

Refuses to render if the slides break the style's limits.
"""
import json
import re
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont, ImageOps

CANVAS = {"tiktok": (1080, 1920), "feed": (1080, 1350), "square": (1080, 1080)}
# Areas the platform UI covers, as (top, right, bottom, left) pixels. Text is
# kept out of them; background images still run full bleed.
UI_SAFE = {"tiktok": (180, 150, 420, 70), "feed": (90, 80, 110, 80), "square": (80, 80, 80, 80)}

ENGINE = Path(__file__).resolve().parent
BANNED_CTA = re.compile(r"\bcomment\b", re.I)
SHEET_SCALE = 0.3
MIN_SHEET_PX = 14  # text must stay at least this tall on the contact sheet
# Captions are long and search-optimized (Andy, Oct 27 2026); see SKILL.md.
CAPTION_LEN = (300, 2200)
CAPTION_TITLE_MAX = 100
CAPTION_LINK = re.compile(r"https?://|www\.|standardtrade\.ai|\.ai/|\.com\b", re.I)


def load_font(spec, size):
    path = Path(spec["path"]).expanduser()
    if not path.is_absolute():
        path = ENGINE.parent / path
    f = ImageFont.truetype(str(path), size, index=spec.get("index", 0))
    if spec.get("weight"):
        f.set_variation_by_name(spec["weight"])
    return f


def wrap(draw, text, f, width):
    lines = []
    for para in text.split("\n"):
        line = ""
        for word in para.split():
            trial = f"{line} {word}".strip()
            if not line or draw.textlength(trial, font=f) <= width:
                line = trial
            else:
                lines.append(line)
                line = word
        lines.append(line)
    return lines


def fit_font(draw, text, fspec, size, min_size, width, height, leading):
    """Largest size from `size` down to `min_size` at which the text fits."""
    while True:
        f = load_font(fspec, size)
        lines = wrap(draw, text, f, width)
        # A single long word (a URL) must also fit on its line.
        widest = max((draw.textlength(w, font=f) for w in text.split()), default=0)
        if (len(lines) * size * leading <= height and widest <= width) or size <= min_size:
            return f, lines
        size -= 4


def background(style, slide, size, base):
    bg = style["background"]
    img = Image.new("RGB", size, bg.get("color", "#000000"))
    image = slide.get("image") or bg.get("image")
    if image:
        path = Path(image)
        if not path.is_absolute():
            path = base / path
        photo = ImageOps.fit(Image.open(path).convert("RGB"), size)
        if bg.get("blur"):
            photo = photo.filter(ImageFilter.GaussianBlur(bg["blur"]))
        img = photo
    elif bg.get("gradient"):
        top, bottom = (Image.new("RGB", (1, 1), c).getpixel((0, 0)) for c in bg["gradient"])
        g = Image.new("RGB", (1, 256))
        for y in range(256):
            g.putpixel((0, y), tuple(int(top[i] + (bottom[i] - top[i]) * y / 255) for i in range(3)))
        img = g.resize(size)
    if bg.get("top_shade"):
        # Darken the top of the image so text there stays readable:
        # [color, strength 0-1, fraction of height it fades over].
        color, strength, frac = bg["top_shade"]
        shade = Image.new("RGB", size, color)
        mask = Image.new("L", (1, 256))
        for y in range(256):
            mask.putpixel((0, y), int(255 * strength * max(0.0, 1 - (y / 255) / frac)))
        img = Image.composite(shade, img, mask.resize(size))
    if bg.get("overlay"):
        color, alpha = bg["overlay"]
        shade = Image.new("RGB", size, color)
        img = Image.blend(img, shade, alpha)
    return img


def draw_text(img, text, tstyle, box):
    """Draw text in one of three treatments: plain, outline (TikTok stroke) or box."""
    d = ImageDraw.Draw(img, "RGBA")
    x0, y0, x1, y1 = box
    width, height = x1 - x0, y1 - y0
    leading = tstyle.get("leading", 1.2)
    if tstyle.get("case") == "upper":
        text = text.upper()
    elif tstyle.get("case") == "lower":
        text = text.lower()
    f, lines = fit_font(d, text, tstyle["font"], tstyle["size"], tstyle.get("min_size", 40),
                        width, height, leading)
    line_h = int(f.size * leading)
    block_h = line_h * len(lines)
    pos = tstyle.get("position", "center")
    top = {"upper": y0, "center": y0 + (height - block_h) // 2, "lower": y1 - block_h}[pos]
    treatment = tstyle.get("treatment", "plain")
    stroke = tstyle.get("stroke_width", 0) if treatment == "outline" else 0
    pad = tstyle.get("box_padding", 18)
    for i, line in enumerate(lines):
        w = d.textlength(line, font=f)
        x = {"left": x0, "center": x0 + (width - w) / 2, "right": x1 - w}[tstyle.get("align", "center")]
        y = top + i * line_h
        if treatment == "box":
            asc, desc = f.getmetrics()
            d.rounded_rectangle((x - pad, y - pad * 0.6, x + w + pad, y + asc + desc + pad * 0.4),
                                radius=tstyle.get("box_radius", 10), fill=tstyle["box_color"])
        d.text((x, y), line, font=f, fill=tstyle["color"],
               stroke_width=stroke, stroke_fill=tstyle.get("stroke_color"))
    return f.size, top + block_h


def rounded_mask(size, radius):
    m = Image.new("L", size, 0)
    ImageDraw.Draw(m).rounded_rectangle((0, 0, size[0] - 1, size[1] - 1), radius=radius, fill=255)
    return m


def draw_chat(img, messages, cs, box, base):
    """Draw an iPhone Messages screen card inside `box`: contact header, blue
    sent bubbles, gray received bubbles, an optional rich link preview."""
    x0, y0, x1, y1 = box
    sc = cs.get("scale", 1.0)
    W = x1 - x0
    font = load_font(cs["font"], int(40 * sc))
    small = load_font(cs["font"], int(26 * sc))
    pad, radius, maxw = int(26 * sc), int(40 * sc), int(W * 0.74)
    d0 = ImageDraw.Draw(img)

    # Lay out first to know the card height.
    rows = []
    for m in messages:
        if m.get("link"):
            rows.append(("link", m, int(420 * sc) if m.get("thumb") else int(360 * sc)))
            continue
        lines = wrap(d0, m["text"], font, maxw - 2 * pad)
        w = max(d0.textlength(l, font=font) for l in lines) + 2 * pad
        h = len(lines) * int(48 * sc) + 2 * int(18 * sc)
        rows.append(("msg", dict(m, lines=lines, w=int(w)), h))
    header_h = int(170 * sc)
    gap = int(10 * sc)
    body_h = sum(h for _, _, h in rows) + gap * len(rows) + int(80 * sc)
    card_h = min(header_h + body_h, y1 - y0)
    card = Image.new("RGB", (W, card_h), cs.get("bg", "#ffffff"))
    d = ImageDraw.Draw(card)

    # Header: avatar + name.
    if cs.get("avatar"):
        av = Image.open(base / cs["avatar"] if not str(cs["avatar"]).startswith("/") else cs["avatar"]).convert("RGB")
        a = int(84 * sc)
        av = av.resize((a, a))
        card.paste(av, ((W - a) // 2, int(22 * sc)), rounded_mask((a, a), a // 2))
    name = cs.get("name", "")
    d.text(((W - d.textlength(name, font=small)) / 2, int(116 * sc)), name, font=small, fill=cs.get("name_color", "#000000"))
    d.line((0, header_h - 2, W, header_h - 2), fill=cs.get("line", "#d1d1d6"), width=2)

    y = header_h + int(24 * sc)
    for kind, m, h in rows:
        me = m.get("me", False)
        if kind == "link":
            lw = maxw
            lx = W - pad - lw if me else pad
            thumb_h = h - int(110 * sc)
            ph = Image.new("RGB", (lw, h), cs.get("them", "#e9e9eb"))
            pd = ImageDraw.Draw(ph)
            if m.get("thumb"):
                # A real frame from the reel, like iMessage's link preview.
                src = Path(m["thumb"]) if str(m["thumb"]).startswith("/") else base / m["thumb"]
                ph.paste(ImageOps.fit(Image.open(src).convert("RGB"), (lw, thumb_h)), (0, 0))
                # Instagram's small reels glyph in the corner.
                g = int(34 * sc)
                gx, gy = lw - g - int(16 * sc), int(16 * sc)
                pd.rounded_rectangle((gx, gy, gx + g, gy + g), radius=int(9 * sc), outline="#ffffff", width=max(2, int(3 * sc)))
                tri = int(12 * sc)
                cx, cy = gx + g // 2 + 2, gy + g // 2 + 2
                pd.polygon([(cx - tri // 2, cy - tri // 2), (cx - tri // 2, cy + tri // 2), (cx + tri // 2, cy)], fill="#ffffff")
            else:
                pd.rectangle((0, 0, lw, thumb_h), fill="#c7c7cc")
            pd.text((int(22 * sc), thumb_h + int(16 * sc)), m["link"], font=small, fill=cs.get("them_ink", "#000"))
            pd.text((int(22 * sc), thumb_h + int(52 * sc)), m.get("domain", "instagram.com"), font=small, fill=cs.get("sub", "#8e8e93"))
            card.paste(ph, (lx, y), rounded_mask((lw, h), radius))
        else:
            bx = W - pad - m["w"] if me else pad
            d.rounded_rectangle((bx, y, bx + m["w"], y + h), radius=radius,
                                fill=cs.get("me", "#0a84ff") if me else cs.get("them", "#e9e9eb"))
            ty = y + int(18 * sc)
            for line in m["lines"]:
                d.text((bx + pad, ty), line, font=font, fill="#ffffff" if me else cs.get("them_ink", "#000000"))
                ty += int(48 * sc)
        y += h + gap
        if y > card_h - int(40 * sc):
            break
    img.paste(card, (x0, y0), rounded_mask((W, card_h), int(48 * sc)))


def caption_errors(caption):
    """Problems with a post caption; empty when it follows the rule."""
    errors = []
    caption = (caption or "").strip()
    lines = caption.splitlines()
    if not lines:
        return ["caption is empty; write a search-optimized caption"]
    if len(lines[0]) > CAPTION_TITLE_MAX:
        errors.append(f"caption title is {len(lines[0])} chars; max {CAPTION_TITLE_MAX}")
    if len(lines) < 3 or lines[1].strip():
        errors.append("caption needs a title line, a blank line, then the body")
    lo, hi = CAPTION_LEN
    if not lo <= len(caption) <= hi:
        errors.append(f"caption is {len(caption)} chars; use {lo} to {hi}")
    if "link in bio" not in caption.lower():
        errors.append("caption needs a CTA line with 'link in bio'")
    tags = re.findall(r"#\w+", caption)
    if not 3 <= len(tags) <= 6:
        errors.append(f"caption has {len(tags)} hashtags; use 3 to 6")
    elif not re.fullmatch(r"(#\w+\s*)+", lines[-1].strip()):
        errors.append("hashtags go on the last line")
    if CAPTION_LINK.search(caption):
        errors.append("caption has a URL or domain; say 'link in bio' instead")
    return errors


def check(style, project):
    errors = []
    limits = style.get("limits", {})
    slides = project["slides"]
    lo, hi = limits.get("slides", [3, 10])
    if not lo <= len(slides) <= hi:
        errors.append(f"{len(slides)} slides; this style allows {lo} to {hi}")
    if slides[0].get("kind") != "hook":
        errors.append("slide 1 must be the hook")
    hook_max = limits.get("hook_words")
    if hook_max and len(slides[0]["text"].split()) > hook_max:
        errors.append(f"hook has {len(slides[0]['text'].split())} words; max {hook_max}")
    product = project.get("product", "").strip().lower()
    if product and product in slides[0]["text"].lower() and not limits.get("allow_product_in_hook"):
        errors.append("hook names the product; the hook earns the swipe, the pitch waits for the CTA")
    kinds = [s.get("kind") for s in slides]
    if kinds.count("cta") != 1 or kinds[-1] != "cta":
        errors.append("use exactly one 'cta' slide, and make it the last")
    word_max = limits.get("words_per_slide")
    for i, s in enumerate(slides, 1):
        n = len((s.get("title", "") + " " + s.get("text", "")).split())
        if word_max and n > word_max:
            errors.append(f"slide {i}: {n} words; max {word_max}")
    if not project.get("allow_comment_cta") and any(
            BANNED_CTA.search(t) for t in [project.get("caption", "")] + [s["text"] for s in slides]):
        errors.append("'comment X' CTA found; only use one if Andy asks (set allow_comment_cta)")
    errors += caption_errors(project.get("caption"))
    return errors


def render(style_path, project_path):
    style_path, project_path = Path(style_path), Path(project_path)
    style = json.loads(style_path.read_text())
    project = json.loads(project_path.read_text())
    errors = check(style, project)
    if errors:
        print("Slides break the style's limits; fix the slides, don't bypass:")
        for e in errors:
            print(" -", e)
        sys.exit(1)

    canvas = style.get("canvas", "tiktok")
    size = CANVAS[canvas]
    st, sr, sb, sl = UI_SAFE[canvas]
    margin = style.get("margin", 0)
    box = (sl + margin, st + margin, size[0] - sr - margin, size[1] - sb - margin)
    out = project_path.parent / "out" / style_path.stem
    out.mkdir(parents=True, exist_ok=True)
    base = project_path.parent

    images = []
    for i, s in enumerate(project["slides"], 1):
        img = background(style, s, size, base)
        tstyle = dict(style["text"])
        tstyle.update(style.get("kinds", {}).get(s["kind"], {}))
        if s.get("chat") and style.get("chat"):
            # Text-conversation slide: an iMessage screen, with the slide's
            # text (if any) as an outlined caption above the bubbles.
            top = st + margin
            if s.get("text"):
                used, top = draw_text(img, s["text"], dict(tstyle, position="upper"), box)
                top += 40
            else:
                used = tstyle["size"]
            draw_chat(img, s["chat"], style["chat"], (box[0], top, box[2], box[3]), base)
        elif s.get("title") and style.get("title"):
            # Title + body layout: a short title, then the body under it,
            # starting at the style's anchor (fraction of the safe area).
            ttl = dict(style["title"], position="upper")
            body = dict(tstyle, position="upper")
            y = int(box[1] + (box[3] - box[1]) * style.get("anchor", 0.0))
            tsize, bottom = draw_text(img, s["title"], ttl, (box[0], y, box[2], box[3]))
            used, _ = draw_text(img, s["text"], body,
                                (box[0], bottom + style.get("gap", 30), box[2], box[3]))
            used = min(used, tsize)
        else:
            used, _ = draw_text(img, s["text"], tstyle, box)
        if used * SHEET_SCALE < MIN_SHEET_PX:
            print(f"Slide {i}: text shrank to {used}px, unreadable in the feed. Cut words or split the slide.")
            sys.exit(1)
        if style.get("counter"):
            c = style["counter"]
            d = ImageDraw.Draw(img)
            f = load_font(c["font"], c["size"])
            label = f"{i}/{len(project['slides'])}"
            d.text((box[2] - d.textlength(label, font=f), st - c["size"] - 10), label,
                   font=f, fill=c["color"])
        img.save(out / f"{i:02d}.png")
        images.append(img)

    scale = SHEET_SCALE
    tw, th = int(size[0] * scale), int(size[1] * scale)
    cols = min(len(images), 5)
    rows = (len(images) + cols - 1) // cols
    sheet = Image.new("RGB", (cols * (tw + 14) + 14, rows * (th + 14) + 14), "#2A2A2A")
    for i, img in enumerate(images):
        r, c = divmod(i, cols)
        sheet.paste(img.resize((tw, th)), (14 + c * (tw + 14), 14 + r * (th + 14)))
    sheet.save(out / "sheet.png")
    (out / "caption.txt").write_text(project["caption"].strip() + "\n")
    print(f"Rendered {len(images)} slides in style '{style_path.stem}' to {out}")


if __name__ == "__main__":
    render(sys.argv[1], sys.argv[2])
