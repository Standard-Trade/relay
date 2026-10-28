# Slides

Write a slideshow as HTML, render it to the JPGs Relay posts, and check it
before it reaches the queue.

1. Make a folder with `slides.html` and `post.json`. Each element with class
   `slide` is one 1080x1920 image. `assets/kit.css` has ready blocks: a white
   box hook (`.tt`), big type (`.big`, `.mid`, `.body`), a tweet card (`.tw`),
   an Apple Notes page (`.notes`), an iMessage thread (`.ios`, `.b.me`,
   `.b.them`) and a CTA band (`.cta`).
2. `node slides/render.mjs <folder>` writes `01.jpg`... and `sheet.jpg`
   (needs Playwright with Chromium).
3. `python3 slides/check.py <folder> [...]` checks the fields, the caption
   rule, 3 to 10 slides, a future time and a 30-minute gap between accounts.
   Set `RELAY_ACCOUNTS=handle.one,handle.two` to also reject unknown accounts.
4. Queue the JPGs and caption in Relay (admin page or `/admin/api/import`).

`post.json`:

```json
{"account": "your.handle", "at": "2026-10-29T02:00:00Z",
 "prop": "the one value prop this account sells",
 "hook": "<slide 1 text>", "caption": "<caption>", "comment": "<first comment>"}
```

Caption rule (`engine/render.py`): a title line of at most 100 characters, a
blank line, 300 to 2200 characters in all, "link in bio", 3 to 6 hashtags on
the last line, no URLs.

`example/` is a real post. Its time is in the past, so `check.py` reports
that until you change `at`.
