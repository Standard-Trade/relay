use anyhow::{bail, Context, Result};
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use relay::config::Config;
use relay::engine::{self, Providers};
use relay::providers::{instagram::Instagram, tiktok, tiktok::TikTok, DryRun};
use relay::store::{self, Store};
use relay::{manifest, media};

const USAGE: &str = "relay: our own slideshow poster

usage: relay [--config relay.json] [--db relay.db] <command>

  import <manifest.json> <YYYY-MM-DD>  queue a day's posts from batch.py
  tick [--dry-run]                    post what is due and allowed, once
  run [--dry-run] [--every SECS]      tick forever (default every 60 s)
  status                              queue, accounts, recent events
  pause <handle> | resume <handle>    stop or restart one account
  stop | start                        stop or restart everything
  requeue <post-id> | drop <post-id>  after checking an 'unknown' post
  login-url <handle>                  TikTok sign-in link for an account
  login <handle> <code>               finish TikTok sign-in (needs TIKTOK_CLIENT_SECRET)
  refresh                             refresh TikTok tokens that expire within a day
  set-token <handle>                  store a token read from stdin (Instagram)
  serve [--setup ADDR] [--public ADDR] [--every SECS]
                                      setup page (default 127.0.0.1:8788), public images and
                                      sign-in callback (127.0.0.1:8787) and the posting loop
  set-password                        set the remote setup-page password (read from stdin)
  push <manifest.json> <YYYY-MM-DD> <https://admin-host>
                                      send a rendered day to a remote Relay (password in RELAY_PASSWORD)
  serve-media [ADDR]                  serve media_dir only (default 127.0.0.1:8787)
";

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg_path = "relay.json".to_string();
    let mut db_path = "relay.db".to_string();
    while args.first().map(|a| a.starts_with("--")).unwrap_or(false) {
        let flag = args.remove(0);
        let val = if args.is_empty() { bail!("{flag} needs a value") } else { args.remove(0) };
        match flag.as_str() {
            "--config" => cfg_path = val,
            "--db" => db_path = val,
            _ => bail!("unknown flag {flag}\n\n{USAGE}"),
        }
    }
    let Some(cmd) = args.first().cloned() else {
        print!("{USAGE}");
        return Ok(());
    };
    let secrets_path = format!("{}.secrets.json", cfg_path.trim_end_matches(".json"));
    if cmd == "set-password" {
        let mut pw = String::new();
        std::io::stdin().read_to_string(&mut pw)?;
        relay::web::Secrets::set_password(&secrets_path, pw.trim_end_matches(['\n', '\r']))?;
        println!("password set");
        return Ok(());
    }
    if cmd == "push" {
        let [path, date, host] = &args[1..] else { bail!("usage: push <manifest.json> <YYYY-MM-DD> <https://admin-host>") };
        return push(path, date, host);
    }
    if cmd == "serve" {
        let flag = |name: &str, default: &str| {
            rest_flag(&args[1..], name).unwrap_or_else(|| default.to_string())
        };
        let setup = flag("--setup", "127.0.0.1:8788");
        let public = flag("--public", "127.0.0.1:8787");
        let every: u64 = flag("--every", "60").parse()?;
        let (c, d, s) = (cfg_path.clone(), db_path.clone(), secrets_path.clone());
        let tick = move || {
            let run = || -> Result<()> {
                if !std::path::Path::new(&c).exists() {
                    return Ok(());
                }
                let cfg = Config::load(&c)?;
                let store = Store::open(&d)?;
                if let Err(e) = refresh_tokens(&cfg, &store, &s, 2 * 3600) {
                    eprintln!("token refresh: {e:#}");
                }
                let tt = TikTok { api_base: cfg.tiktok.as_ref().map(|t| t.api_base.clone()).unwrap_or_else(|| "https://open.tiktokapis.com".into()) };
                let ig = Instagram { api_base: cfg.instagram.as_ref().map(|i| i.api_base.clone()).unwrap_or_else(|| "https://graph.instagram.com/v23.0".into()), poll_secs: 5, max_polls: 24 };
                let dr = DryRun;
                let r = engine::tick(&cfg, &store, &Providers { tiktok: &tt, instagram: &ig, dryrun: &dr }, now())?;
                if r.sent + r.failed > 0 || r.stopped {
                    eprintln!("{r:?}");
                }
                Ok(())
            };
            if let Err(e) = run() {
                eprintln!("tick error: {e:#}");
            }
        };
        return relay::web::serve(
            relay::web::Paths { config: cfg_path, db: db_path, secrets: secrets_path },
            &setup,
            &public,
            every,
            tick,
        );
    }
    let cfg = Config::load(&cfg_path)?;
    let store = Store::open(&db_path)?;
    let rest = &args[1..];
    let dry = rest.iter().any(|a| a == "--dry-run");

    let tt = TikTok { api_base: cfg.tiktok.as_ref().map(|t| t.api_base.clone()).unwrap_or_else(|| "https://open.tiktokapis.com".into()) };
    let ig = Instagram {
        api_base: cfg.instagram.as_ref().map(|i| i.api_base.clone()).unwrap_or_else(|| "https://graph.instagram.com/v23.0".into()),
        poll_secs: 5,
        max_polls: 24,
    };
    let dr = DryRun;
    let providers = if dry {
        Providers { tiktok: &dr, instagram: &dr, dryrun: &dr }
    } else {
        Providers { tiktok: &tt, instagram: &ig, dryrun: &dr }
    };

    match cmd.as_str() {
        "import" => {
            let [path, date] = &rest[..] else { bail!("usage: import <manifest.json> <YYYY-MM-DD>") };
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
            let n = manifest::import(&cfg, &store, &text, date)?;
            println!("queued {n} new posts for {date}");
        }
        "tick" => println!("{:?}", engine::tick(&cfg, &store, &providers, now())?),
        "run" => {
            let every: u64 = rest
                .iter()
                .position(|a| a == "--every")
                .and_then(|i| rest.get(i + 1))
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(60);
            loop {
                match engine::tick(&cfg, &store, &providers, now()) {
                    Ok(r) if r.sent + r.failed > 0 || r.stopped => eprintln!("{r:?}"),
                    Ok(_) => {}
                    Err(e) => eprintln!("tick error: {e:#}"),
                }
                std::thread::sleep(std::time::Duration::from_secs(every));
            }
        }
        "status" => status(&cfg, &store)?,
        "pause" | "resume" => {
            let [h] = rest else { bail!("usage: {cmd} <handle>") };
            store.set_paused(h, cmd == "pause", "by hand")?;
            println!("{h} {}d", cmd);
        }
        "stop" | "start" => {
            store.set_stopped(cmd == "stop")?;
            println!("engine {}", if cmd == "stop" { "stopped" } else { "started" });
        }
        "requeue" | "drop" => {
            let [id] = rest else { bail!("usage: {cmd} <post-id>") };
            if cmd == "requeue" {
                store.requeue(id, now())?
            } else {
                store.set_status(id, store::DROPPED, "dropped by hand")?
            }
            println!("{id}: {cmd}");
        }
        "login-url" => {
            let [h] = rest else { bail!("usage: login-url <handle>") };
            let t = cfg.tiktok.as_ref().context("no tiktok section in config")?;
            println!("{}", tiktok::authorize_url(&t.client_key, &t.redirect_uri, h, "user.info.basic,video.upload,video.publish"));
        }
        "login" => {
            let [h, code] = rest else { bail!("usage: login <handle> <code>") };
            let t = cfg.tiktok.as_ref().context("no tiktok section in config")?;
            let secret = relay::web::Secrets::tiktok_secret(&secrets_path).context("set TIKTOK_CLIENT_SECRET or save it on the setup page")?;
            let tok = tiktok::exchange(&t.api_base, &[
                ("client_key", &t.client_key),
                ("client_secret", &secret),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", &t.redirect_uri),
            ])?;
            store.save_token(h, &tok.access_token, tok.refresh_token.as_deref(), tok.expires_in.map(|s| now() + s))?;
            store.set_paused(h, false, "")?;
            println!("{h}: signed in");
        }
        "refresh" => {
            for h in refresh_tokens(&cfg, &store, &secrets_path, 86_400)? {
                println!("{h}: refreshed");
            }
        }
        "set-token" => {
            let [h] = rest else { bail!("usage: set-token <handle>  (token on stdin)") };
            let mut tok = String::new();
            std::io::stdin().read_to_string(&mut tok)?;
            store.save_token(h, tok.trim(), None, None)?;
            println!("{h}: token saved");
        }
        "serve-media" => {
            let addr = rest.first().map(String::as_str).unwrap_or("127.0.0.1:8787");
            media::serve(std::path::Path::new(&cfg.media_dir), addr)?;
        }
        _ => bail!("unknown command {cmd}\n\n{USAGE}"),
    }
    Ok(())
}

/// Refresh TikTok tokens that expire within `within` seconds. TikTok access
/// tokens last 24 hours; refresh tokens last a year.
fn refresh_tokens(cfg: &Config, store: &Store, secrets_path: &str, within: i64) -> Result<Vec<String>> {
    let Some(t) = cfg.tiktok.as_ref() else { return Ok(vec![]) };
    let mut done = vec![];
    for a in cfg.accounts.iter().filter(|a| a.platform == "tiktok") {
        let Some((_, Some(refresh), exp)) = store.token(&a.handle)? else { continue };
        if exp.map(|e| e - now() > within).unwrap_or(false) {
            continue;
        }
        let secret = relay::web::Secrets::tiktok_secret(secrets_path).context("set TIKTOK_CLIENT_SECRET or save it on the setup page")?;
        let tok = tiktok::exchange(&t.api_base, &[
            ("client_key", &t.client_key),
            ("client_secret", &secret),
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
        ])?;
        store.save_token(&a.handle, &tok.access_token, tok.refresh_token.as_deref(), tok.expires_in.map(|s| now() + s))?;
        done.push(a.handle.clone());
    }
    Ok(done)
}

/// Send a rendered day to the hosted Relay (Cloudflare Worker): each slide
/// is converted to JPEG here, uploaded by content hash, then the day's posts
/// are queued referring to those images.
fn push(path: &str, date: &str, host: &str) -> Result<()> {
    let pw = std::env::var("RELAY_PASSWORD").context("set RELAY_PASSWORD")?;
    let host = host.trim_end_matches('/');
    let login = ureq::post(&format!("{host}/admin/api/login"))
        .set("X-Relay", "1")
        .send_json(serde_json::json!({ "password": pw }))
        .map_err(|e| anyhow::anyhow!("sign-in failed: {e}"))?;
    let cookie = login.header("Set-Cookie").context("no session cookie")?;
    let token = cookie.split(';').next().and_then(|kv| kv.strip_prefix("relay_session=")).context("bad cookie")?.to_string();
    let call_err = |e: ureq::Error| match e {
        ureq::Error::Status(c, r) => anyhow::anyhow!("HTTP {c}: {}", r.into_string().unwrap_or_default()),
        e => anyhow::anyhow!(e),
    };
    let tmp = std::env::temp_dir().join("relay-push");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let mut posts = vec![];
    let mut uploaded = 0;
    for e in entries {
        let mut keys = vec![];
        for i in e["images"].as_array().context("images")? {
            let key = media::publish_image(std::path::Path::new(i.as_str().unwrap_or("")), &tmp)?;
            let bytes = std::fs::read(tmp.join(&key))?;
            ureq::put(&format!("{host}/admin/api/media/{key}"))
                .set("X-Relay", "1")
                .set("X-Relay-Session", &token)
                .set("Content-Type", "image/jpeg")
                .send_bytes(&bytes)
                .map_err(call_err)?;
            uploaded += 1;
            keys.push(serde_json::Value::String(key));
        }
        let mut e = e.clone();
        e["images"] = serde_json::Value::Array(keys);
        posts.push(e);
    }
    let r: serde_json::Value = ureq::post(&format!("{host}/admin/api/import"))
        .set("X-Relay", "1")
        .set("X-Relay-Session", &token)
        .send_json(serde_json::json!({ "date": date, "posts": posts }))
        .map_err(call_err)?
        .into_json()?;
    println!("uploaded {uploaded} images; queued {} posts for {date} on {host}", r["queued"]);
    Ok(())
}

fn rest_flag(rest: &[String], name: &str) -> Option<String> {
    rest.iter().position(|a| a == name).and_then(|i| rest.get(i + 1)).cloned()
}

fn status(cfg: &Config, store: &Store) -> Result<()> {
    let t = now();
    println!("engine: {}", if store.stopped()? { "STOPPED" } else { "running" });
    for a in &cfg.accounts {
        let s = store.account_state(&a.handle, t, cfg.utc_offset())?;
        let cap = relay::policy::effective_cap(&a.limits(), s.started_at, t, cfg.utc_offset());
        println!(
            "  {:<24} {:<9} {:<6} today {}/{}  pending {}  {}",
            a.handle, a.platform, a.mode, s.posted_today, cap, s.pending,
            if s.paused { "PAUSED" } else { "" }
        );
    }
    for st in [store::QUEUED, store::SENDING, store::SENT, store::UNKNOWN, store::FAILED] {
        let n = store.by_status(st)?.len();
        if n > 0 {
            println!("  {st}: {n}");
        }
    }
    let mut stmt = store.conn.prepare("SELECT at, account, post, kind, detail FROM events ORDER BY at DESC LIMIT 10")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?))
    })?;
    for r in rows {
        let (at, acct, post, kind, detail) = r?;
        println!("  {at} {acct} {post} {kind} {}", detail.unwrap_or_default());
    }
    Ok(())
}
