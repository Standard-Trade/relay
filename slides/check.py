"""Check queued posts before they reach Relay.

    python3 slides/check.py <post folder> [...]

Each post folder holds post.json, slides.html and the rendered NN.jpg files.
post.json: {"account": "<Relay handle>", "at": "<ISO time with offset>",
            "hook": "<slide 1 text>", "caption": "...", "comment": "...",
            "prop": "<the account's value prop>"}
Fails on caption-rule breaks (engine/render.py), a missing field, an unknown
account (when RELAY_ACCOUNTS is set), a time in the past, or two accounts posting under 30 minutes apart.
"""
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "engine"))
from render import caption_errors  # noqa: E402

# Comma-separated Relay handles, e.g. RELAY_ACCOUNTS=shop.one,shop.two
ACCOUNTS = {a.strip() for a in os.environ.get("RELAY_ACCOUNTS", "").split(",") if a.strip()}
GAP_MIN = 30


def check(folder):
    errors = []
    try:
        post = json.loads((folder / "post.json").read_text())
    except (OSError, ValueError) as e:
        return [f"post.json: {e}"], None
    for field in ("account", "at", "hook", "caption", "comment", "prop"):
        if not str(post.get(field) or "").strip():
            errors.append(f"{field} missing")
    if ACCOUNTS and post.get("account") not in ACCOUNTS:
        errors.append(f"account {post.get('account')!r} is not a Relay account")
    try:
        at = datetime.fromisoformat(post["at"])
        if at.tzinfo is None:
            errors.append("at needs a UTC offset")
            at = None
        elif at < datetime.now(timezone.utc):
            errors.append(f"at {post['at']} is in the past")
    except (KeyError, ValueError):
        errors.append("at is not an ISO time")
        at = None
    errors += caption_errors(post.get("caption"))
    jpgs = sorted(folder.glob("[0-9][0-9].jpg"))
    if not 3 <= len(jpgs) <= 10:
        errors.append(f"{len(jpgs)} rendered slides; run slides/render.mjs (3 to 10)")
    return errors, (post.get("account"), at) if at else None


def main(folders):
    bad = 0
    times = []
    for f in map(Path, folders):
        errors, slot = check(f)
        if slot:
            times.append((slot[1], slot[0], f.name))
        for e in errors:
            print(f"{f}: {e}")
        bad += len(errors)
    times.sort()
    for (t1, a1, p1), (t2, a2, p2) in zip(times, times[1:]):
        if a1 != a2 and (t2 - t1).total_seconds() < GAP_MIN * 60:
            print(f"{p1} ({a1}) and {p2} ({a2}) are under {GAP_MIN} min apart")
            bad += 1
    print(f"{len(folders)} posts, {bad} problems")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
