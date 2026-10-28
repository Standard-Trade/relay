# Relay

Relay posts TikTok photo slideshows on a schedule, across several accounts,
without tripping the patterns that get new accounts throttled. It also serves
the Standard Grow site at grow.standardtrade.ai.

It never writes posts. You queue them (from the admin page or the import API),
and a once-a-minute cron decides what may go out now.

## What's here

| Folder | What it is |
| --- | --- |
| `relay/` | The posting policy and scheduler as a plain Rust crate, with a local server, property tests and Kani proofs. `relay/public/` is the static site. |
| `relay-worker/` | The same policy on Cloudflare Workers: D1 for the queue, KV for slide images, a cron trigger, the admin page and the TikTok Content Posting API (inbox mode). |
| `slides/` | An HTML slide kit: write slides in HTML, render them to 1080x1920 JPGs with Playwright, check captions and spacing before queueing. |

## The posting policy

`relay/src/policy.rs` holds every rule, and the Worker compiles the same file:

- a daily cap per account, ramping up for new accounts
- a minimum gap between two posts on one account
- a limit on posts waiting in TikTok's inbox
- small random jitter on each scheduled time
- a post that breaks a rule waits for the next allowed time; nothing is dropped

Run the tests with `cargo test` in `relay/` and in `relay-worker/`.

## Slides

```
node slides/render.mjs slides/example      # NN.jpg + sheet.jpg
python3 slides/check.py slides/example     # caption rule, fields, 30-minute gap
```

See `slides/README.md` for the post format and the classes in `slides/assets/kit.css`.

## Deploying

Cloudflare Workers Builds deploys `relay-worker` on every push to `main`:

- root directory: `relay-worker`
- build command: `bash cf-build.sh` (installs Rust and worker-build, then compiles)
- deploy command: `npx wrangler d1 migrations apply standard-grow --remote && npx wrangler deploy`
- build variable: `WORKER_PREBUILT=1`

To run your own copy, change the name, route, D1 and KV ids in
`relay-worker/wrangler.toml`, then set the secrets the Worker reads
(`ADMIN_PASSWORD`, `TIKTOK_CLIENT_SECRET`) with `wrangler secret put`, and
enter your TikTok app's client key in the admin settings.

## License

MIT
