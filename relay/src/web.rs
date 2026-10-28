//! `relay serve`: the setup page, the public media + sign-in callback, and
//! the posting loop, in one process.
//!
//! Two listeners on purpose:
//! - setup (default 127.0.0.1:8788): the onboarding page and its JSON API.
//!   Only answers requests addressed to localhost, and every write needs the
//!   `X-Relay: 1` header, which a page on another site can't send without a
//!   CORS preflight we never approve.
//! - public (default 127.0.0.1:8787, put your tunnel in front of it): only
//!   slide images and the TikTok sign-in callback.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::{AccountConfig, Config, TikTokConfig};
use crate::media;
use crate::policy::effective_cap;
use crate::providers::tiktok;
use crate::store::{self, Store};

const PAGE: &str = include_str!("setup.html");

pub struct Paths {
    pub config: String,
    pub db: String,
    pub secrets: String,
}

/// Secrets live in their own file (mode 600), never in relay.json.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Secrets {
    #[serde(default)]
    pub tiktok_client_secret: String,
    /// "salt$sha256(salt + password)" for the remote setup login.
    #[serde(default)]
    pub admin_password_hash: String,
}

impl Secrets {
    pub fn load(path: &str) -> Secrets {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }
    pub fn set_password(path: &str, password: &str) -> Result<()> {
        if password.chars().count() < 12 {
            return Err(anyhow!("use at least 12 characters"));
        }
        let mut s = Secrets::load(path);
        let salt = random_hex(16);
        s.admin_password_hash = format!("{salt}${}", hash_password(&salt, password));
        s.save(path)
    }

    pub fn check_password(path: &str, password: &str) -> bool {
        let s = Secrets::load(path);
        let Some((salt, want)) = s.admin_password_hash.split_once('$') else { return false };
        constant_eq(hash_password(salt, password).as_bytes(), want.as_bytes())
    }

    pub fn save(&self, path: &str) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    /// The TikTok client secret: env var first, then the secrets file.
    pub fn tiktok_secret(path: &str) -> Option<String> {
        std::env::var("TIKTOK_CLIENT_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| Some(Secrets::load(path).tiktok_client_secret).filter(|s| !s.is_empty()))
    }
}

fn hash_password(salt: &str, password: &str) -> String {
    use sha2::{Digest, Sha256};
    // 100k rounds of salted SHA-256: slow enough to blunt guessing if the
    // secrets file ever leaks, with no extra dependency.
    let mut h = Sha256::digest(format!("{salt}{password}").as_bytes()).to_vec();
    for _ in 0..100_000 {
        h = Sha256::digest(&h).to_vec();
    }
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Pending sign-ins: OAuth state → handle.
type Pending = Arc<Mutex<HashMap<String, String>>>;

pub struct App {
    pub paths: Paths,
    pending: Pending,
    /// Serializes config writes from the page.
    lock: Mutex<()>,
    /// Remote setup sessions: token → expiry (unix seconds).
    sessions: Mutex<HashMap<String, i64>>,
    /// Failed logins in the current minute, to slow guessing.
    failures: Mutex<(i64, u32)>,
    /// Host name the setup page also answers on (behind the tunnel), e.g.
    /// grow-admin.example.com. Requests on it need a login.
    pub admin_host: Option<String>,
}

impl App {
    pub fn new(paths: Paths) -> App {
        App {
            paths,
            pending: Arc::new(Mutex::new(HashMap::new())),
            lock: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new((0, 0)),
            admin_host: std::env::var("RELAY_ADMIN_HOST").ok().filter(|h| !h.is_empty()),
        }
    }

    fn store(&self) -> Result<Store> {
        Store::open(&self.paths.db)
    }

    fn config(&self) -> Result<Config> {
        Config::load_or_blank(&self.paths.config)
    }

    /// Everything the setup page shows.
    pub fn state(&self) -> Result<Value> {
        let cfg = self.config()?;
        let store = self.store()?;
        let t = now();
        let off = cfg.utc_offset();
        let mut accounts = vec![];
        for a in &cfg.accounts {
            let s = store.account_state(&a.handle, t, off)?;
            let connected = a.platform == "dryrun" || store.token(&a.handle)?.is_some();
            accounts.push(json!({
                "config": a,
                "connected": connected,
                "paused": s.paused,
                "today": s.posted_today,
                "cap_today": effective_cap(&a.limits(), s.started_at, t, off),
                "pending": s.pending,
            }));
        }
        let mut queue = vec![];
        for (p, _, _) in store.by_status(store::QUEUED)?.into_iter().take(50) {
            queue.push(json!({"id": p.id, "account": p.account, "at": p.scheduled_at, "hook": p.hook}));
        }
        let mut attention = vec![];
        for st in [store::UNKNOWN, store::FAILED] {
            for (p, _, _) in store.by_status(st)? {
                attention.push(json!({"id": p.id, "account": p.account, "status": st, "hook": p.hook}));
            }
        }
        Ok(json!({
            "settings": {
                "utc_offset_hours": cfg.utc_offset_hours,
                "media_base_url": cfg.media_base_url,
                "jitter_minutes": cfg.jitter_minutes,
                "tiktok_client_key": cfg.tiktok.as_ref().map(|t| t.client_key.clone()).unwrap_or_default(),
                "tiktok_secret_set": Secrets::tiktok_secret(&self.paths.secrets).is_some(),
                "redirect_uri": redirect_uri(&cfg),
            },
            "stopped": store.stopped()?,
            "accounts": accounts,
            "queue": queue,
            "attention": attention,
            "now": t,
        }))
    }

    fn edit_config(&self, f: impl FnOnce(&mut Config) -> Result<()>) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut cfg = self.config()?;
        f(&mut cfg)?;
        cfg.save(&self.paths.config)
    }

    pub fn save_settings(&self, v: &Value) -> Result<()> {
        if let Some(secret) = v["tiktok_client_secret"].as_str().filter(|s| !s.is_empty()) {
            let mut s = Secrets::load(&self.paths.secrets);
            s.tiktok_client_secret = secret.trim().into();
            s.save(&self.paths.secrets)?;
        }
        self.edit_config(|cfg| {
            if let Some(h) = v["utc_offset_hours"].as_i64() {
                if !(-12..=14).contains(&h) {
                    return Err(anyhow!("UTC offset must be between -12 and 14"));
                }
                cfg.utc_offset_hours = h;
            }
            if let Some(u) = v["media_base_url"].as_str() {
                cfg.media_base_url = u.trim().trim_end_matches('/').into();
            }
            if let Some(j) = v["jitter_minutes"].as_i64() {
                cfg.jitter_minutes = j.clamp(0, 30);
            }
            if let Some(k) = v["tiktok_client_key"].as_str().filter(|s| !s.is_empty()) {
                let redirect = format!("{}/callback", cfg.media_base_url);
                cfg.tiktok = Some(TikTokConfig {
                    client_key: k.trim().into(),
                    redirect_uri: redirect,
                    api_base: cfg.tiktok.as_ref().map(|t| t.api_base.clone()).unwrap_or_else(|| "https://open.tiktokapis.com".into()),
                });
            } else if let Some(t) = cfg.tiktok.as_mut() {
                t.redirect_uri = format!("{}/callback", cfg.media_base_url);
            }
            Ok(())
        })
    }

    pub fn upsert_account(&self, v: &Value) -> Result<()> {
        let a: AccountConfig = serde_json::from_value(v.clone()).context("account fields")?;
        self.edit_config(|cfg| {
            match cfg.accounts.iter_mut().find(|x| x.handle == a.handle) {
                Some(x) => *x = a,
                None => cfg.accounts.push(a),
            }
            Ok(())
        })
    }

    pub fn delete_account(&self, handle: &str) -> Result<()> {
        self.edit_config(|cfg| {
            cfg.accounts.retain(|a| a.handle != handle);
            Ok(())
        })
    }

    pub fn login_url(&self, handle: &str) -> Result<String> {
        let cfg = self.config()?;
        cfg.account(handle).ok_or_else(|| anyhow!("no account {handle}"))?;
        let t = cfg.tiktok.as_ref().ok_or_else(|| anyhow!("add your TikTok app key in step 1 first"))?;
        if cfg.media_base_url.is_empty() {
            return Err(anyhow!("add your public URL in step 1 first"));
        }
        let state = format!("{handle}.{}", random_hex(12));
        self.pending.lock().unwrap().insert(state.clone(), handle.to_string());
        Ok(tiktok::authorize_url(&t.client_key, &t.redirect_uri, &state, "user.info.basic,video.upload,video.publish"))
    }

    /// Finish a TikTok sign-in. Returns the handle that was connected.
    pub fn callback(&self, state: &str, code: &str) -> Result<String> {
        let handle = self
            .pending
            .lock()
            .unwrap()
            .remove(state)
            .ok_or_else(|| anyhow!("this sign-in link expired; start again from the setup page"))?;
        let cfg = self.config()?;
        let t = cfg.tiktok.as_ref().ok_or_else(|| anyhow!("no TikTok app configured"))?;
        let secret = Secrets::tiktok_secret(&self.paths.secrets).ok_or_else(|| anyhow!("add the TikTok client secret in step 1"))?;
        let tok = tiktok::exchange(
            &t.api_base,
            &[
                ("client_key", &t.client_key),
                ("client_secret", &secret),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", &t.redirect_uri),
            ],
        )?;
        let store = self.store()?;
        store.save_token(&handle, &tok.access_token, tok.refresh_token.as_deref(), tok.expires_in.map(|s| now() + s))?;
        store.set_paused(&handle, false, "")?;
        Ok(handle)
    }

    pub fn action(&self, v: &Value) -> Result<()> {
        let store = self.store()?;
        let handle = v["handle"].as_str().unwrap_or("");
        match v["do"].as_str().unwrap_or("") {
            "pause" => store.set_paused(handle, true, "paused from setup page"),
            "resume" => store.set_paused(handle, false, ""),
            "stop" => store.set_stopped(true),
            "start" => store.set_stopped(false),
            "token" => {
                let tok = v["token"].as_str().unwrap_or("").trim();
                if tok.is_empty() {
                    return Err(anyhow!("paste the token first"));
                }
                store.save_token(handle, tok, None, None)
            }
            "requeue" => store.requeue(v["id"].as_str().unwrap_or(""), now()),
            "drop" => store.set_status(v["id"].as_str().unwrap_or(""), store::DROPPED, "dropped from setup page"),
            other => Err(anyhow!("unknown action {other}")),
        }
    }
}

impl App {
    /// Check the admin password; returns a new session token.
    pub fn login(&self, password: &str) -> Result<String> {
        let t = now();
        {
            let mut f = self.failures.lock().unwrap();
            if t / 60 != f.0 {
                *f = (t / 60, 0);
            }
            if f.1 >= 10 {
                return Err(anyhow!("too many attempts; wait a minute"));
            }
        }
        if !Secrets::check_password(&self.paths.secrets, password) {
            self.failures.lock().unwrap().1 += 1;
            std::thread::sleep(std::time::Duration::from_millis(800));
            return Err(anyhow!("wrong password"));
        }
        let token = random_hex(32);
        let mut s = self.sessions.lock().unwrap();
        s.retain(|_, exp| *exp > t);
        s.insert(token.clone(), t + 12 * 3600);
        Ok(token)
    }

    pub fn session_ok(&self, token: &str) -> bool {
        self.sessions.lock().unwrap().get(token).map(|exp| *exp > now()).unwrap_or(false)
    }

    /// Queue posts pushed from another machine: {date, posts: [{post,
    /// account, slot, caption, hook, images: [base64 png]}]}.
    pub fn import(&self, v: &Value) -> Result<usize> {
        let date = v["date"].as_str().ok_or_else(|| anyhow!("date missing"))?;
        crate::schedule::parse_date(date)?;
        let base = std::path::Path::new(&self.paths.config).parent().unwrap_or(std::path::Path::new(".")).join("queue").join(date);
        let mut entries = vec![];
        for p in v["posts"].as_array().ok_or_else(|| anyhow!("posts missing"))? {
            let post = p["post"].as_str().unwrap_or("");
            if post.is_empty() || !post.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) || post.contains("..") {
                return Err(anyhow!("bad post name {post:?}"));
            }
            let dir = base.join(post);
            std::fs::create_dir_all(&dir)?;
            let mut paths = vec![];
            for (i, img) in p["images"].as_array().ok_or_else(|| anyhow!("{post}: images missing"))?.iter().enumerate() {
                let bytes = decode_base64(img.as_str().unwrap_or("")).ok_or_else(|| anyhow!("{post}: image {i} is not base64"))?;
                image::load_from_memory(&bytes).map_err(|e| anyhow!("{post}: image {i}: {e}"))?;
                let f = dir.join(format!("{:02}.png", i + 1));
                std::fs::write(&f, bytes)?;
                paths.push(f.to_string_lossy().to_string());
            }
            entries.push(json!({"post": post, "account": p["account"], "slot": p["slot"], "caption": p["caption"], "hook": p["hook"], "images": paths}));
        }
        let cfg = self.config()?;
        let store = self.store()?;
        crate::manifest::import(&cfg, &store, &Value::Array(entries).to_string(), date)
    }
}

pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let clean: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    if clean.len() % 4 == 1 {
        return None;
    }
    Some(out)
}

const LOGIN_PAGE: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Relay sign-in</title>
<style>:root{--bg:#fbfbfa;--ink:#1c1c1c;--line:#e6e5e1;--accent:#5b5bd6}@media (prefers-color-scheme:dark){:root{--bg:#111112;--ink:#ececec;--line:#2a2a2d;--accent:#8b8bf0}}
body{margin:0;background:var(--bg);color:var(--ink);font:13px/1.45 -apple-system,BlinkMacSystemFont,"Inter",sans-serif}form{max-width:320px;margin:18vh auto;padding:0 16px;display:flex;flex-direction:column;gap:10px}
h1{font-size:15px;margin:0}input,button{font:inherit;padding:7px 9px;border-radius:6px;border:1px solid var(--line);background:transparent;color:var(--ink)}button{background:var(--accent);border-color:var(--accent);color:#fff;cursor:pointer}#e{color:#c0392b;min-height:1em}</style></head>
<body><form id="f"><h1>Relay</h1><input id="p" type="password" placeholder="Password" autofocus autocomplete="current-password"><button>Sign in</button><div id="e"></div></form>
<script>f.onsubmit=async e=>{e.preventDefault();const r=await fetch('/api/login',{method:'POST',headers:{'X-Relay':'1','Content-Type':'application/json'},body:JSON.stringify({password:p.value})});if(r.ok)location.reload();else{const j=await r.json().catch(()=>({}));document.getElementById('e').textContent=j.error||'Sign-in failed'}}</script></body></html>"#;

pub fn redirect_uri(cfg: &Config) -> String {
    if cfg.media_base_url.is_empty() {
        String::new()
    } else {
        format!("{}/callback", cfg.media_base_url)
    }
}

/// Is this request addressed to the setup server on this machine?
pub fn local_host(host: Option<&str>) -> bool {
    let Some(h) = host else { return false };
    let name = h.rsplit_once(':').map(|(n, _)| n).unwrap_or(h);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

fn header<'a>(req: &'a tiny_http::Request, name: &str) -> Option<&'a str> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}

fn respond(req: tiny_http::Request, code: u16, ctype: &str, body: String) {
    let h = tiny_http::Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()).unwrap();
    let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(code).with_header(h));
}

fn json_result(req: tiny_http::Request, r: Result<Value>) {
    match r {
        Ok(v) => respond(req, 200, "application/json", v.to_string()),
        Err(e) => respond(req, 400, "application/json", json!({"error": format!("{e:#}")}).to_string()),
    }
}

fn query(url: &str) -> HashMap<String, String> {
    let q = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Handle one request to the setup server.
pub fn handle_setup(app: &App, mut req: tiny_http::Request) {
    let host = header(&req, "Host").map(str::to_string);
    let local = local_host(host.as_deref());
    let remote = !local && app.admin_host.is_some() && host.as_deref().map(|h| h.split(':').next() == app.admin_host.as_deref()).unwrap_or(false);
    if !local && !remote {
        return respond(req, 403, "text/plain", "setup is not available on this address".into());
    }
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let is_post = *req.method() == tiny_http::Method::Post;
    if is_post && header(&req, "X-Relay") != Some("1") {
        return respond(req, 403, "text/plain", "missing X-Relay header".into());
    }
    let limit = if path == "/api/import" { 200 << 20 } else { 1 << 20 };
    let mut body = String::new();
    if is_post {
        let _ = req.as_reader().take(limit).read_to_string(&mut body);
    }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if remote {
        if (is_post, path.as_str()) == (true, "/api/login") {
            return match app.login(v["password"].as_str().unwrap_or("")) {
                Ok(token) => {
                    let cookie = format!("relay_session={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=43200");
                    let h = tiny_http::Header::from_bytes(&b"Set-Cookie"[..], cookie.as_bytes()).unwrap();
                    let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
                    let _ = req.respond(tiny_http::Response::from_string("{}").with_header(h).with_header(ct));
                }
                Err(e) => respond(req, 401, "application/json", json!({"error": format!("{e:#}")}).to_string()),
            };
        }
        let token = header(&req, "Cookie")
            .and_then(|c| c.split(';').map(str::trim).find_map(|kv| kv.strip_prefix("relay_session=")))
            .or_else(|| header(&req, "X-Relay-Session"))
            .map(str::to_string);
        if !token.map(|t| app.session_ok(&t)).unwrap_or(false) {
            return if path == "/" {
                respond(req, 200, "text/html; charset=utf-8", LOGIN_PAGE.to_string())
            } else {
                respond(req, 401, "application/json", json!({"error": "sign in first"}).to_string())
            };
        }
    }
    match (is_post, path.as_str()) {
        (false, "/") => respond(req, 200, "text/html; charset=utf-8", PAGE.to_string()),
        (true, "/api/import") => json_result(req, app.import(&v).and_then(|n| Ok(json!({ "queued": n })))),
        (false, "/api/state") => json_result(req, app.state()),
        (false, "/api/login-url") => {
            let h = query(&url).remove("handle").unwrap_or_default();
            json_result(req, app.login_url(&h).map(|u| json!({ "url": u })))
        }
        (true, "/api/settings") => json_result(req, app.save_settings(&v).and_then(|_| app.state())),
        (true, "/api/account") => json_result(req, app.upsert_account(&v).and_then(|_| app.state())),
        (true, "/api/account/delete") => {
            json_result(req, app.delete_account(v["handle"].as_str().unwrap_or("")).and_then(|_| app.state()))
        }
        (true, "/api/action") => json_result(req, app.action(&v).and_then(|_| app.state())),
        _ => respond(req, 404, "text/plain", "not found".into()),
    }
}

/// Resolve a public site path (home, terms, privacy, logos, platform
/// verification files) to a file in `site_dir`.
pub fn site_file(site_dir: &Path, path: &str) -> Option<(PathBuf, &'static str)> {
    let name = match path {
        "/" => "index.html".to_string(),
        "/terms" | "/privacy" => format!("{}.html", &path[1..]),
        p => p.trim_start_matches('/').to_string(),
    };
    let safe = !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && !name.starts_with('_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !safe || name.contains("..") {
        return None;
    }
    let ctype = match name.rsplit('.').next()? {
        "html" => "text/html; charset=utf-8",
        "png" => "image/png",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    };
    let p = site_dir.join(&name);
    p.is_file().then_some((p, ctype))
}

/// Handle one request to the public server: the small public site, images
/// and the sign-in callback.
pub fn handle_public(app: &App, media_dir: &Path, req: tiny_http::Request) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("");
    let site_dir = media_dir.parent().map(|p| p.join("public")).unwrap_or_default();
    if let (tiny_http::Method::Get | tiny_http::Method::Head, Some((file, ctype))) = (req.method(), site_file(&site_dir, path)) {
        if let Ok(f) = std::fs::File::open(file) {
            let h = tiny_http::Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()).unwrap();
            let _ = req.respond(tiny_http::Response::from_file(f).with_header(h));
            return;
        }
    }
    if path == "/callback" {
        let q = query(&url);
        let page = |title: &str, msg: &str| {
            format!("<!doctype html><meta charset=utf-8><title>{title}</title><body style=\"font:16px -apple-system,sans-serif;max-width:32rem;margin:4rem auto\"><h1 style=\"font-size:20px\">{title}</h1><p>{msg}</p>")
        };
        if let Some(err) = q.get("error") {
            return respond(req, 400, "text/html", page("Sign-in cancelled", &html_escape(err)));
        }
        let result = match (q.get("state"), q.get("code")) {
            (Some(s), Some(c)) => app.callback(s, c),
            _ => Err(anyhow!("missing code")),
        };
        return match result {
            Ok(h) => respond(req, 200, "text/html", page("Connected", &format!("@{} is connected. You can close this tab and go back to the setup page.", html_escape(&h)))),
            Err(e) => respond(req, 400, "text/html", page("Sign-in failed", &html_escape(&format!("{e:#}")))),
        };
    }
    match (req.method(), media::resolve(media_dir, path)) {
        (tiny_http::Method::Get | tiny_http::Method::Head, Some(file)) => match std::fs::File::open(file) {
            Ok(f) => {
                let h = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"image/jpeg"[..]).unwrap();
                let _ = req.respond(tiny_http::Response::from_file(f).with_header(h));
            }
            Err(_) => respond(req, 404, "text/plain", String::new()),
        },
        _ => respond(req, 404, "text/plain", String::new()),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Run both servers and the posting loop until killed.
pub fn serve(paths: Paths, setup_addr: &str, public_addr: &str, tick_every: u64, tick: impl Fn() + Send + 'static) -> Result<()> {
    let media_dir = PathBuf::from(Config::load_or_blank(&paths.config)?.media_dir);
    let app = Arc::new(App::new(paths));
    let setup = tiny_http::Server::http(setup_addr).map_err(|e| anyhow!("binding {setup_addr}: {e}"))?;
    let public = tiny_http::Server::http(public_addr).map_err(|e| anyhow!("binding {public_addr}: {e}"))?;
    eprintln!("setup page:   http://{setup_addr}");
    eprintln!("public (images + sign-in callback): http://{public_addr}  <- point your tunnel here");
    let a = app.clone();
    std::thread::spawn(move || {
        for req in public.incoming_requests() {
            handle_public(&a, &media_dir, req);
        }
    });
    std::thread::spawn(move || loop {
        tick();
        std::thread::sleep(std::time::Duration::from_secs(tick_every));
    });
    for req in setup.incoming_requests() {
        handle_setup(&app, req);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_hosts_reach_setup() {
        assert!(local_host(Some("127.0.0.1:8788")));
        assert!(local_host(Some("localhost:8788")));
        assert!(local_host(Some("[::1]:8788")));
        assert!(!local_host(Some("media.example.com")));
        assert!(!local_host(Some("127.0.0.1.evil.com:8788")));
        assert!(!local_host(None));
    }

    #[test]
    fn query_decoding() {
        let q = query("/callback?code=a%2Fb+c&state=h.12&scopes=x");
        assert_eq!(q["code"], "a/b c");
        assert_eq!(q["state"], "h.12");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }

    #[test]
    fn site_files_are_contained() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["index.html", "terms.html", "logo-32.png", "tiktokABC.txt", "secret.json"] {
            std::fs::write(dir.path().join(f), "x").unwrap();
        }
        assert!(site_file(dir.path(), "/").is_some());
        assert!(site_file(dir.path(), "/terms").is_some());
        assert!(site_file(dir.path(), "/logo-32.png").is_some());
        assert!(site_file(dir.path(), "/tiktokABC.txt").is_some());
        for bad in ["/secret.json", "/../relay.json", "/privacy", "/.env", "/a/b.png"] {
            assert!(site_file(dir.path(), bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn base64_roundtrip() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8gd29ybGQ").unwrap(), b"hello world");
        assert!(decode_base64("a").is_none());
        assert!(decode_base64("a$bc").is_none());
    }

    #[test]
    fn password_check() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json").to_string_lossy().to_string();
        assert!(Secrets::set_password(&p, "short").is_err());
        Secrets::set_password(&p, "correct horse battery").unwrap();
        assert!(Secrets::check_password(&p, "correct horse battery"));
        assert!(!Secrets::check_password(&p, "correct horse batterY"));
        assert!(!std::fs::read_to_string(&p).unwrap().contains("horse"));
    }

    #[test]
    fn escapes_html() {
        assert_eq!(html_escape("<b>&\""), "&lt;b&gt;&amp;&quot;");
    }
}
