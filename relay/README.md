# relay: our own slideshow poster

A Rust replacement for Postiz, built for the slideshow growth engine. It reads the `manifest.json` that `engine/batch.py` writes, schedules each post into its account's slot (with jitter), and publishes photo slideshows to TikTok and carousels to Instagram. It enforces the account-safety rules on every post.

## Safety rules (proved with Kani, see `src/policy.rs`)
1. A stopped engine posts nothing.
2. A paused account posts nothing.
3. No account posts more than its cap in a local day. The cap is the smallest of the account's own cap, its ramp step (2, 4, 6, then 8 a day by account week, configurable), TikTok's 15 a day and our hard max of 10.
4. Two posts from one account are at least `min_gap_minutes` apart.
5. In TikTok inbox mode, never more than 5 drafts are pending at once (TikTok's own limit).
6. Nothing posts before its slot.

Any spam-risk error from TikTok (`spam_risk_*`, `rate_limit_exceeded`, HTTP 429) or Instagram (codes 4, 9, 17, 32, 368, 613) stops the whole engine and pauses that account until a person runs `relay start` / `relay resume`. A bad token pauses only that account. A post is claimed in the database before the platform call, so a crash can never post twice; it shows up as `unknown` for a person to check.

## Verification
- `cargo kani --lib`: 7 proofs over every possible input (any time, timezone, limits, account state). The key one is inductive: if an account is within today's cap, it stays within the cap after any later step, so the cap holds on every day of an unbounded run.
- `cargo test`: 30 tests. These include end-to-end runs against a fake TikTok and a fake Instagram over real HTTP with real SQLite, and 2,000 random multi-day schedules (proptest).

## Posting modes
- **TikTok inbox** (`"mode": "inbox"`, scope `video.upload`): the slideshow lands in the account's TikTok inbox. The owner taps the notification, picks a trending sound, and posts. This works before TikTok audits our app.
- **TikTok direct** (`"mode": "direct"`, scope `video.publish`): posts publicly with auto-added music. Needs our TikTok app to pass TikTok's Direct Post audit; until then TikTok makes these posts private.
- **Instagram** (`"platform": "instagram"`): carousels of 2 to 20 images through the Instagram Graph API. Works for professional accounts that have a role on our Meta app. Use the 1080x1350 (`feed`) styles.

## Start here
```
cargo build --release
./target/release/relay serve
```
Open http://127.0.0.1:8788 for the setup page. It walks through three steps: connection (time zone, public URL, TikTok app key and secret), accounts (add each one, then Connect: TikTok opens its sign-in; Instagram takes a pasted token), and the schedule (each account's daily times on a timeline, and the posts coming up). It also shows posts that need a look, and has pause/resume per account and a stop-all switch. The same process serves images and the sign-in callback on 127.0.0.1:8787 (point your tunnel there) and runs the posting loop every minute. The setup page only answers on this computer; the tunnel side serves nothing but images and the callback. The TikTok secret is stored in `relay.secrets.json` (mode 600), never in `relay.json`.

## Setup details
1. **TikTok app**: create an app at developers.tiktok.com with the Content Posting API, scopes `video.upload` (and `video.publish` for direct). Set the redirect URI. Verify the media domain under "URL properties" (TikTok only pulls photos from a verified domain).
2. **Media host**: `relay serve-media 127.0.0.1:8787` serves the image folder. Put a Cloudflare Tunnel (or any HTTPS proxy) on a subdomain you own, e.g. `media.<yourdomain>`, and set `media_base_url` to it.
3. **Config**: copy `relay.example.json` to `relay.json` and fill in accounts.
4. **Sign in each TikTok account**: `relay login-url <handle>`, open the link signed in as that account, copy `code` from the redirect, then `TIKTOK_CLIENT_SECRET=... relay login <handle> <code>`. For Instagram: `relay set-token <handle> < token.txt`.
5. **Daily**: `python3 engine/batch.py relay/relay.json queue/<date>/`, then `relay import queue/<date>/manifest.json <date>`. Keep `relay run` running (or cron `relay tick` every minute) and `relay refresh` daily.

Try it with no accounts: `relay --config relay.example.json tick --dry-run`.
