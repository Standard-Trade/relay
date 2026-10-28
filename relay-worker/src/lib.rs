//! Relay on Cloudflare Workers.
//!
//! The posting policy is the same file the native Relay uses
//! (`relay/src/policy.rs`), so the Kani proofs cover this deployment too.
//! What changes is the plumbing: D1 instead of SQLite, KV instead of a media
//! folder (KV, which the free plan includes without extra setup), a once-a-minute cron trigger instead of a loop, and fetch()
//! instead of ureq.
//!
//! Public (grow.standardtrade.ai):
//!   /, /terms, /privacy, logos, TikTok verification file  -> static assets
//!   /m/<hash>.jpg   slide images from Workers KV (TikTok and Instagram pull these)
//!   /callback       TikTok sign-in callback
//! Setup (password protected):
//!   /admin          setup page; /admin/api/* its API, plus media upload and
//!                   import for `relay push`

#[path = "../../relay/src/policy.rs"]
pub mod policy;
#[path = "../../relay/src/schedule.rs"]
pub mod schedule;

use policy::{AccountState, Decision, ErrorClass, Limits, Mode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::wasm_bindgen::JsValue;
use worker::*;

const SETUP_PAGE: &str = include_str!("../../relay/src/setup.html");
const LOGIN_PAGE: &str = include_str!("login.html");

const QUEUED: &str = "queued";
const SENDING: &str = "sending";
const SENT: &str = "sent";
const PUBLISHED: &str = "published";
const FAILED: &str = "failed";
const DROPPED: &str = "dropped";
const UNKNOWN: &str = "unknown";
/// Instagram: containers created, waiting for Meta to finish processing.
const IG_BUILDING: &str = "ig_building";

// ---------------------------------------------------------------- config

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AccountConfig {
    pub handle: String,
    pub platform: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub daily_cap: u32,
    #[serde(default = "default_ramp")]
    pub ramp: [u32; 4],
    #[serde(default = "default_gap")]
    pub min_gap_minutes: i64,
    #[serde(default = "default_privacy")]
    pub privacy_level: String,
    #[serde(default)]
    pub ig_user_id: Option<String>,
    #[serde(default)]
    pub slots: Vec<String>,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub angle: String,
    /// The product this account promotes, shown as a tag in the admin.
    #[serde(default)]
    pub product: String,
    /// Admin color for this account, one of the named swatches.
    #[serde(default)]
    pub color: String,
}
fn default_mode() -> String {
    "inbox".into()
}
fn default_ramp() -> [u32; 4] {
    [2, 4, 6, 8]
}
fn default_gap() -> i64 {
    90
}
fn default_privacy() -> String {
    "PUBLIC_TO_EVERYONE".into()
}

impl AccountConfig {
    fn validate(&self) -> std::result::Result<(), String> {
        if self.handle.is_empty() || !self.handle.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
            return Err(format!("handle {:?} may only use letters, digits, _ and .", self.handle));
        }
        if !["tiktok", "instagram", "dryrun"].contains(&self.platform.as_str()) {
            return Err("platform must be tiktok, instagram or dryrun".into());
        }
        if !["inbox", "direct"].contains(&self.mode.as_str()) {
            return Err("mode must be inbox or direct".into());
        }
        if self.platform == "instagram" && self.ig_user_id.as_deref().unwrap_or("").is_empty() {
            return Err("instagram accounts need ig_user_id".into());
        }
        if self.min_gap_minutes < 0 {
            return Err("min_gap_minutes can't be negative".into());
        }
        for s in &self.slots {
            schedule::parse_slot(s).map_err(|e| format!("slot {s}: {e}"))?;
        }
        Ok(())
    }
    fn mode(&self) -> Mode {
        if self.platform == "tiktok" && self.mode == "inbox" {
            Mode::Inbox
        } else {
            Mode::Direct
        }
    }
    fn limits(&self) -> Limits {
        Limits {
            daily_cap: self.daily_cap,
            ramp: self.ramp,
            platform_max: if self.platform == "tiktok" { policy::TIKTOK_DAILY_MAX } else { policy::HARD_MAX },
            min_gap_secs: self.min_gap_minutes * 60,
            pending_max: policy::TIKTOK_PENDING_MAX,
        }
    }
}

fn now() -> i64 {
    (Date::now().as_millis() / 1000) as i64
}

fn n(x: i64) -> JsValue {
    JsValue::from_f64(x as f64)
}
fn s(x: &str) -> JsValue {
    JsValue::from_str(x)
}
fn opt(x: Option<&str>) -> JsValue {
    x.map(s).unwrap_or(JsValue::NULL)
}

// ---------------------------------------------------------------- store (D1)

struct Db(D1Database);

#[derive(Deserialize)]
struct PostRow {
    id: String,
    account: String,
    scheduled_at: f64,
    images: String,
    caption: String,
    hook: String,
    #[serde(default)]
    publish_id: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    settings: Option<String>,
}

#[derive(Deserialize)]
struct Count {
    n: f64,
}

#[derive(Deserialize)]
struct MaybeNum {
    v: Option<f64>,
}

#[derive(Deserialize)]
struct Kv {
    value: String,
}

#[derive(Deserialize)]
struct AcctRow {
    config: String,
    paused: f64,
    started_at: Option<f64>,
}

#[derive(Deserialize)]
struct TokenRow {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<f64>,
}

impl Db {
    async fn run(&self, sql: &str, args: &[JsValue]) -> Result<()> {
        self.0.prepare(sql).bind(args)?.run().await?;
        Ok(())
    }
    async fn all<T: for<'a> Deserialize<'a>>(&self, sql: &str, args: &[JsValue]) -> Result<Vec<T>> {
        self.0.prepare(sql).bind(args)?.all().await?.results::<T>()
    }
    async fn first<T: for<'a> Deserialize<'a>>(&self, sql: &str, args: &[JsValue]) -> Result<Option<T>> {
        self.0.prepare(sql).bind(args)?.first::<T>(None).await
    }

    async fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.first::<Kv>("SELECT value FROM settings WHERE key = ?1", &[s(key)]).await?.map(|k| k.value))
    }
    async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.run(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            &[s(key), s(value)],
        )
        .await
    }
    async fn utc_offset(&self) -> Result<i64> {
        Ok(self.setting("utc_offset_hours").await?.and_then(|v| v.parse::<i64>().ok()).unwrap_or(-7) * 3600)
    }
    async fn stopped(&self) -> Result<bool> {
        Ok(self.setting("stopped").await?.as_deref() == Some("1"))
    }

    async fn accounts(&self) -> Result<Vec<(AccountConfig, bool, Option<i64>)>> {
        let rows = self.all::<AcctRow>("SELECT config, paused, started_at FROM accounts ORDER BY handle", &[]).await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                serde_json::from_str::<AccountConfig>(&r.config).ok().map(|c| (c, r.paused != 0.0, r.started_at.map(|x| x as i64)))
            })
            .collect())
    }
    async fn account(&self, handle: &str) -> Result<Option<AccountConfig>> {
        Ok(self
            .first::<AcctRow>("SELECT config, paused, started_at FROM accounts WHERE handle = ?1", &[s(handle)])
            .await?
            .and_then(|r| serde_json::from_str(&r.config).ok()))
    }

    /// Policy state for an account, derived from the posts table.
    async fn account_state(&self, handle: &str, t: i64, off: i64) -> Result<AccountState> {
        let row = self.first::<AcctRow>("SELECT config, paused, started_at FROM accounts WHERE handle = ?1", &[s(handle)]).await?;
        let (paused, started_at) = row.map(|r| (r.paused != 0.0, r.started_at.map(|x| x as i64))).unwrap_or((false, None));
        let today = policy::day_of(t, off);
        let day_start = today * policy::SECS_PER_DAY - off;
        let posted_today = self
            .first::<Count>(
                "SELECT COUNT(*) AS n FROM posts WHERE account = ?1 AND sent_at IS NOT NULL AND sent_at >= ?2 AND sent_at < ?3",
                &[s(handle), n(day_start), n(day_start + policy::SECS_PER_DAY)],
            )
            .await?
            .map(|c| c.n as u32)
            .unwrap_or(0);
        let last_post_at = self
            .first::<MaybeNum>("SELECT MAX(sent_at) AS v FROM posts WHERE account = ?1", &[s(handle)])
            .await?
            .and_then(|m| m.v.map(|x| x as i64));
        let pending = self
            .first::<Count>(
                "SELECT COUNT(*) AS n FROM posts WHERE account = ?1 AND mode = 'inbox' AND status = ?2 AND sent_at > ?3",
                &[s(handle), s(SENT), n(t - policy::SECS_PER_DAY)],
            )
            .await?
            .map(|c| c.n as u32)
            .unwrap_or(0);
        Ok(AccountState { paused, started_at: started_at.unwrap_or(t), day: today, posted_today, last_post_at, pending })
    }

    async fn set_paused(&self, handle: &str, paused: bool, reason: &str) -> Result<()> {
        self.run("UPDATE accounts SET paused = ?2, reason = ?3 WHERE handle = ?1", &[s(handle), n(paused as i64), s(reason)]).await
    }
    async fn set_status(&self, id: &str, status: &str, detail: &str) -> Result<()> {
        self.run("UPDATE posts SET status = ?2, detail = ?3 WHERE id = ?1", &[s(id), s(status), s(detail)]).await
    }
    async fn requeue(&self, id: &str, at: i64) -> Result<()> {
        self.run(
            "UPDATE posts SET status = ?2, scheduled_at = ?3, sent_at = NULL, mode = NULL, publish_id = NULL WHERE id = ?1",
            &[s(id), s(QUEUED), n(at)],
        )
        .await
    }
    async fn log(&self, account: &str, post: &str, kind: &str, detail: &str) -> Result<()> {
        self.run(
            "INSERT INTO events (at, account, post, kind, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[n(now()), s(account), s(post), s(kind), s(detail)],
        )
        .await
    }
    async fn token(&self, handle: &str) -> Result<Option<TokenRow>> {
        self.first("SELECT access_token, refresh_token, expires_at FROM tokens WHERE handle = ?1", &[s(handle)]).await
    }
    async fn save_token(&self, handle: &str, access: &str, refresh: Option<&str>, expires_at: Option<i64>) -> Result<()> {
        self.run(
            "INSERT INTO tokens (handle, access_token, refresh_token, expires_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(handle) DO UPDATE SET access_token = ?2, refresh_token = COALESCE(?3, refresh_token), expires_at = ?4",
            &[s(handle), s(access), opt(refresh), expires_at.map(n).unwrap_or(JsValue::NULL)],
        )
        .await
    }
    async fn posts(&self, status: &str, limit: i64) -> Result<Vec<PostRow>> {
        self.all(
            "SELECT id, account, scheduled_at, images, caption, hook, publish_id, mode, comment, settings FROM posts WHERE status = ?1 ORDER BY scheduled_at, id LIMIT ?2",
            &[s(status), n(limit)],
        )
        .await
    }
}

// ---------------------------------------------------------------- platforms

struct Http {
    status: u16,
    body: Value,
    text: String,
}

async fn http(method: Method, url: &str, headers: &[(&str, &str)], body: Option<String>) -> Result<Http> {
    let h = Headers::new();
    for (k, v) in headers {
        h.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(h);
    if let Some(b) = body {
        init.with_body(Some(JsValue::from_str(&b)));
    }
    let mut resp = Fetch::Request(Request::new_with_init(url, &init)?).send().await?;
    let status = resp.status_code();
    let text = resp.text().await.unwrap_or_default();
    Ok(Http { status, body: serde_json::from_str(&text).unwrap_or(Value::Null), text })
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// TikTok error codes → what the engine should do (same table as native).
fn tiktok_class(code: &str, status: u16) -> ErrorClass {
    match code {
        "spam_risk_too_many_posts" | "spam_risk_too_many_pending_share" | "spam_risk_user_banned_from_posting" | "spam_risk"
        | "rate_limit_exceeded" | "reached_active_user_cap" => ErrorClass::SpamRisk,
        "access_token_invalid" | "scope_not_authorized" | "token_not_authorized_for_specified_scope" => ErrorClass::Auth,
        "url_ownership_unverified" | "invalid_params" | "privacy_level_option_mismatch" | "picture_size_check_failed"
        | "file_format_check_failed" | "app_version_check_failed" | "unaudited_client_can_only_post_to_private_accounts" => {
            ErrorClass::BadPost
        }
        _ => match status {
            429 => ErrorClass::SpamRisk,
            401 | 403 => ErrorClass::Auth,
            _ => ErrorClass::Transient,
        },
    }
}

fn meta_class(code: i64, subcode: i64) -> ErrorClass {
    match code {
        4 | 17 | 32 | 613 | 9 | 368 => ErrorClass::SpamRisk,
        190 | 10 | 200 => ErrorClass::Auth,
        100 | 36003 | 352 | 9004 => ErrorClass::BadPost,
        _ if (2_207_000..2_208_000).contains(&subcode) => ErrorClass::BadPost,
        _ => ErrorClass::Transient,
    }
}

type PubResult = std::result::Result<String, (ErrorClass, String)>;

async fn tiktok_post(api: &str, token: &str, mode: Mode, urls: &[String], title: &str, caption: &str, settings: &Value) -> PubResult {
    let mut post_info = json!({
        "title": title.chars().take(80).collect::<String>(),
        "description": caption.chars().take(2000).collect::<String>(),
    });
    let post_mode = if mode == Mode::Direct {
        // Every value here was chosen by the creator on the review screen.
        let Some(privacy) = settings["privacy_level"].as_str() else {
            return Err((ErrorClass::BadPost, "no approved privacy level".into()));
        };
        post_info["privacy_level"] = json!(privacy);
        post_info["disable_comment"] = json!(settings["disable_comment"].as_bool().unwrap_or(false));
        post_info["brand_content_toggle"] = json!(settings["brand_content_toggle"].as_bool().unwrap_or(false));
        post_info["brand_organic_toggle"] = json!(settings["brand_organic_toggle"].as_bool().unwrap_or(false));
        post_info["auto_add_music"] = json!(true);
        "DIRECT_POST"
    } else {
        "MEDIA_UPLOAD"
    };
    let body = json!({
        "media_type": "PHOTO", "post_mode": post_mode, "post_info": post_info,
        "source_info": {"source": "PULL_FROM_URL", "photo_images": urls, "photo_cover_index": 0}
    });
    let r = http(
        Method::Post,
        &format!("{api}/v2/post/publish/content/init/"),
        &[("Authorization", &format!("Bearer {token}")), ("Content-Type", "application/json; charset=UTF-8")],
        Some(body.to_string()),
    )
    .await
    .map_err(|e| (ErrorClass::Transient, e.to_string()))?;
    let code = r.body["error"]["code"].as_str().unwrap_or(if r.status == 200 { "ok" } else { "" });
    if code == "ok" && r.status < 300 {
        r.body["data"]["publish_id"].as_str().map(String::from).ok_or((ErrorClass::Transient, format!("no publish_id: {}", r.text)))
    } else {
        Err((tiktok_class(code, r.status), format!("HTTP {} {code}: {}", r.status, r.text)))
    }
}

/// The creator's current posting options, read fresh before each review.
async fn tiktok_creator_info(api: &str, token: &str) -> std::result::Result<Value, String> {
    let r = http(
        Method::Post,
        &format!("{api}/v2/post/publish/creator_info/query/"),
        &[("Authorization", &format!("Bearer {token}")), ("Content-Type", "application/json; charset=UTF-8")],
        Some("{}".into()),
    )
    .await
    .map_err(|e| e.to_string())?;
    let code = r.body["error"]["code"].as_str().unwrap_or("");
    if r.status >= 300 || (code != "ok" && !code.is_empty()) {
        return Err(format!("TikTok creator info: HTTP {} {code}", r.status));
    }
    Ok(r.body["data"].clone())
}

/// Status of a send, and the public post id once TikTok has published it.
async fn tiktok_status(api: &str, token: &str, publish_id: &str) -> Option<(String, Option<String>)> {
    let r = http(
        Method::Post,
        &format!("{api}/v2/post/publish/status/fetch/"),
        &[("Authorization", &format!("Bearer {token}")), ("Content-Type", "application/json; charset=UTF-8")],
        Some(json!({ "publish_id": publish_id }).to_string()),
    )
    .await
    .ok()?;
    let mut status = r.body["data"]["status"].as_str()?.to_string();
    if let Some(reason) = r.body["data"]["fail_reason"].as_str().filter(|x| !x.is_empty()) {
        status = format!("{status}: {reason}");
    }
    let post_id = r.body["data"]["publicaly_available_post_id"]
        .as_array()
        .and_then(|a| a.first())
        .map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()));
    Some((status, post_id))
}

#[derive(Debug, PartialEq)]
struct VideoStats {
    id: String,
    title: String,
    description: String,
    create_time: i64,
    views: i64,
    likes: i64,
    comments: i64,
    shares: i64,
}

fn parse_videos(body: &Value) -> Vec<VideoStats> {
    body["data"]["videos"]
        .as_array()
        .map(|vs| {
            vs.iter()
                .map(|v| VideoStats {
                    id: v["id"].as_str().map(String::from).unwrap_or_else(|| v["id"].to_string()),
                    title: v["title"].as_str().unwrap_or("").to_string(),
                    description: v["video_description"].as_str().unwrap_or("").to_string(),
                    create_time: v["create_time"].as_i64().unwrap_or(0),
                    views: v["view_count"].as_i64().unwrap_or(0),
                    likes: v["like_count"].as_i64().unwrap_or(0),
                    comments: v["comment_count"].as_i64().unwrap_or(0),
                    shares: v["share_count"].as_i64().unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The account's recent posts (newest first, up to 40) with their counts.
async fn tiktok_videos(api: &str, token: &str) -> Vec<VideoStats> {
    let url = format!("{api}/v2/video/list/?fields=id,title,video_description,create_time,view_count,like_count,comment_count,share_count");
    let auth = format!("Bearer {token}");
    let mut out = vec![];
    let mut cursor: Option<i64> = None;
    for _ in 0..2 {
        let body = match cursor {
            Some(c) => json!({ "max_count": 20, "cursor": c }),
            None => json!({ "max_count": 20 }),
        };
        let Ok(r) = http(Method::Post, &url, &[("Authorization", &auth), ("Content-Type", "application/json")], Some(body.to_string())).await
        else {
            break;
        };
        out.extend(parse_videos(&r.body));
        if !r.body["data"]["has_more"].as_bool().unwrap_or(false) {
            break;
        }
        cursor = r.body["data"]["cursor"].as_i64();
    }
    out
}

/// Which of the account's videos is this post? By post id when TikTok gave
/// one; otherwise the first video posted after we sent it whose title or
/// description starts with our hook (inbox posts are finished by hand, so
/// their id isn't known to us).
fn match_video<'a>(videos: &'a [VideoStats], post_id: Option<&str>, hook: &str, caption: &str, sent_at: i64) -> Option<&'a VideoStats> {
    if let Some(id) = post_id.filter(|s| !s.is_empty()) {
        return videos.iter().find(|v| v.id == id);
    }
    // Inbox posts are finished by hand, so the title or the caption may be
    // the one that survives. Either prefix identifies the post.
    let prefix = |t: &str| t.chars().take(40).collect::<String>().to_lowercase();
    let keys: Vec<String> = [prefix(hook), prefix(caption)].into_iter().filter(|k| k.len() >= 12).collect();
    if keys.is_empty() {
        return None;
    }
    videos
        .iter()
        .filter(|v| v.create_time >= sent_at - 60 && v.create_time <= sent_at + 3 * 86_400)
        .filter(|v| {
            let (t, d) = (v.title.to_lowercase(), v.description.to_lowercase());
            keys.iter().any(|k| t.starts_with(k.as_str()) || d.starts_with(k.as_str()))
        })
        .min_by_key(|v| v.create_time)
}

fn ig_err(r: &Http) -> (ErrorClass, String) {
    let c = r.body["error"]["code"].as_i64().unwrap_or(0);
    let sc = r.body["error"]["error_subcode"].as_i64().unwrap_or(0);
    let class = if r.status == 429 { ErrorClass::SpamRisk } else { meta_class(c, sc) };
    (class, format!("HTTP {}: {}", r.status, r.text))
}

/// Instagram step 1: create item containers and the carousel container.
/// Returns the carousel container id; publishing happens on a later tick.
async fn ig_build(api: &str, token: &str, user: &str, urls: &[String], caption: &str) -> PubResult {
    if !(2..=20).contains(&urls.len()) {
        return Err((ErrorClass::BadPost, "carousels take 2 to 20 images".into()));
    }
    let media = format!("{api}/{user}/media");
    let ct = [("Content-Type", "application/x-www-form-urlencoded")];
    let mut kids = vec![];
    for u in urls {
        let r = http(Method::Post, &media, &ct, Some(form(&[("image_url", u), ("is_carousel_item", "true"), ("access_token", token)])))
            .await
            .map_err(|e| (ErrorClass::Transient, e.to_string()))?;
        match r.body["id"].as_str() {
            Some(id) if r.status < 300 => kids.push(id.to_string()),
            _ => return Err(ig_err(&r)),
        }
    }
    let r = http(
        Method::Post,
        &media,
        &ct,
        Some(form(&[("media_type", "CAROUSEL"), ("children", &kids.join(",")), ("caption", caption), ("access_token", token)])),
    )
    .await
    .map_err(|e| (ErrorClass::Transient, e.to_string()))?;
    match r.body["id"].as_str() {
        Some(id) if r.status < 300 => Ok(id.to_string()),
        _ => Err(ig_err(&r)),
    }
}

/// Instagram step 2: publish once Meta has finished the container.
/// Ok(None) means not ready yet.
async fn ig_publish(api: &str, token: &str, user: &str, container: &str) -> std::result::Result<Option<String>, (ErrorClass, String)> {
    let r = http(Method::Get, &format!("{api}/{container}?fields=status_code&access_token={token}"), &[], None)
        .await
        .map_err(|e| (ErrorClass::Transient, e.to_string()))?;
    match r.body["status_code"].as_str().unwrap_or("") {
        "FINISHED" => {}
        "ERROR" | "EXPIRED" => return Err((ErrorClass::BadPost, format!("container: {}", r.text))),
        _ => return Ok(None),
    }
    let r = http(
        Method::Post,
        &format!("{api}/{user}/media_publish"),
        &[("Content-Type", "application/x-www-form-urlencoded")],
        Some(form(&[("creation_id", container), ("access_token", token)])),
    )
    .await
    .map_err(|e| (ErrorClass::Transient, e.to_string()))?;
    match r.body["id"].as_str() {
        Some(id) if r.status < 300 => Ok(Some(id.to_string())),
        _ => Err(ig_err(&r)),
    }
}

// ---------------------------------------------------------------- the tick

struct Ctx {
    db: Db,
    public_url: String,
    tiktok_api: String,
    ig_api: String,
    tiktok_key: Option<String>,
    tiktok_secret: Option<String>,
    /// PostHog project key and capture host, for the growth dashboard.
    posthog: Option<(String, String)>,
}

/// An event for PostHog. Relay's events have no person profile; the
/// distinct id groups them (per account, or the visitor who signed up).
fn ph_event(event: &str, distinct_id: &str, t: i64, mut props: Value) -> Value {
    props["$process_person_profile"] = json!(false);
    props["$lib"] = json!("relay");
    json!({ "event": event, "distinct_id": distinct_id, "timestamp": iso_time(t), "properties": props })
}

/// Send events to PostHog. Best effort: analytics never blocks posting.
async fn ph_send(c: &Ctx, events: Vec<Value>) {
    let Some((key, host)) = &c.posthog else { return };
    if events.is_empty() {
        return;
    }
    let body = json!({ "api_key": key, "batch": events }).to_string();
    match http(Method::Post, &format!("{host}/batch/"), &[("Content-Type", "application/json")], Some(body)).await {
        Ok(r) if r.status < 300 => {}
        Ok(r) => console_error!("posthog: HTTP {}: {}", r.status, r.text),
        Err(e) => console_error!("posthog: {e}"),
    }
}

/// Unix seconds as an ISO 8601 UTC time (days-from-civil, Howard Hinnant).
fn iso_time(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

async fn ctx(env: &Env) -> Result<Ctx> {
    let db = Db(env.d1("DB")?);
    let tiktok_secret = match env.secret("TIKTOK_CLIENT_SECRET") {
        Ok(v) => Some(v.to_string()),
        Err(_) => db.setting("tiktok_client_secret").await?,
    }
    .filter(|s| !s.is_empty());
    let tiktok_key = db.setting("tiktok_client_key").await?.filter(|s| !s.is_empty());
    Ok(Ctx {
        db,
        public_url: env.var("PUBLIC_URL").map(|v| v.to_string()).unwrap_or_default(),
        tiktok_api: env.var("TIKTOK_API").map(|v| v.to_string()).unwrap_or_else(|_| "https://open.tiktokapis.com".into()),
        ig_api: env.var("IG_API").map(|v| v.to_string()).unwrap_or_else(|_| "https://graph.instagram.com/v23.0".into()),
        tiktok_key,
        tiktok_secret,
        posthog: match (env.var("POSTHOG_KEY"), env.var("POSTHOG_HOST")) {
            (Ok(k), Ok(h)) if !k.to_string().is_empty() => Some((k.to_string(), h.to_string())),
            _ => None,
        },
    })
}

async fn handle_error(c: &Ctx, post: &PostRow, acct: &AccountConfig, state: AccountState, class: ErrorClass, msg: &str) -> Result<()> {
    let mut st = state;
    let stopped = policy::on_error(&mut st, class, c.db.stopped().await?);
    c.db.log(&acct.handle, &post.id, "error", msg).await?;
    match class {
        ErrorClass::Transient => c.db.requeue(&post.id, now() + 15 * 60).await?,
        ErrorClass::BadPost => c.db.set_status(&post.id, FAILED, msg).await?,
        ErrorClass::Auth | ErrorClass::SpamRisk => {
            c.db.requeue(&post.id, post.scheduled_at as i64).await?;
            c.db.set_paused(&acct.handle, true, msg).await?;
        }
    }
    if stopped {
        c.db.set_setting("stopped", "1").await?;
    }
    Ok(())
}

async fn tick(c: &Ctx) -> Result<()> {
    let t = now();
    let off = c.db.utc_offset().await?;
    refresh_tokens(c, t).await?;
    for p in c.db.posts(SENDING, 50).await? {
        c.db.set_status(&p.id, UNKNOWN, "interrupted while sending; check the account").await?;
    }
    let due = c
        .db
        .all::<PostRow>(
            "SELECT id, account, scheduled_at, images, caption, hook, publish_id, mode, comment, settings FROM posts WHERE status = ?1 AND scheduled_at <= ?2 ORDER BY scheduled_at, id LIMIT 20",
            &[s(QUEUED), n(t)],
        )
        .await?;
    for post in due {
        if c.db.stopped().await? {
            break;
        }
        let Some(acct) = c.db.account(&post.account).await? else {
            c.db.set_status(&post.id, DROPPED, "account not configured").await?;
            continue;
        };
        let mode = acct.mode();
        // A direct post goes out only with the creator's approved settings.
        if acct.platform == "tiktok" && mode == Mode::Direct && post.settings.is_none() {
            continue;
        }
        let state = c.db.account_state(&acct.handle, t, off).await?;
        if policy::decide(false, &state, &acct.limits(), mode, t, post.scheduled_at as i64, off) != Decision::Post {
            continue;
        }
        let token = match c.db.token(&acct.handle).await? {
            Some(tk) => tk.access_token,
            None if acct.platform == "dryrun" => String::new(),
            None => {
                c.db.set_paused(&acct.handle, true, "not connected").await?;
                continue;
            }
        };
        let keys: Vec<String> = serde_json::from_str(&post.images).unwrap_or_default();
        let urls: Vec<String> = keys.iter().map(|k| format!("{}/m/{k}", c.public_url)).collect();
        // Claim first: a crash after this can never post twice.
        c.db.run(
            "UPDATE posts SET status = ?2, mode = ?3, sent_at = ?4 WHERE id = ?1 AND status = ?5",
            &[s(&post.id), s(SENDING), s(if mode == Mode::Inbox { "inbox" } else { "direct" }), n(t), s(QUEUED)],
        )
        .await?;
        if c.db.stopped().await? {
            c.db.requeue(&post.id, post.scheduled_at as i64).await?;
            break;
        }
        let (status, result) = match acct.platform.as_str() {
            "tiktok" => {
                let settings: Value = post.settings.as_deref().and_then(|x| serde_json::from_str(x).ok()).unwrap_or(Value::Null);
                (SENT, tiktok_post(&c.tiktok_api, &token, mode, &urls, &post.hook, &post.caption, &settings).await)
            }
            "instagram" => {
                let user = acct.ig_user_id.clone().unwrap_or_default();
                (IG_BUILDING, ig_build(&c.ig_api, &token, &user, &urls, &post.caption).await)
            }
            _ => (PUBLISHED, Ok(format!("dry-{}", post.id))),
        };
        match result {
            Ok(id) => {
                c.db.run("UPDATE posts SET status = ?2, publish_id = ?3 WHERE id = ?1", &[s(&post.id), s(status), s(&id)]).await?;
                c.db.log(&acct.handle, &post.id, "sent", &id).await?;
                let props = json!({ "account": acct.handle, "product": acct.product, "platform": acct.platform, "mode": if mode == Mode::Inbox { "inbox" } else { "direct" }, "post": post.id, "hook": post.hook });
                ph_send(c, vec![ph_event("relay_post_sent", &format!("relay:{}", acct.handle), t, props)]).await;
            }
            Err((class, msg)) => handle_error(c, &post, &acct, state, class, &msg).await?,
        }
    }
    // Instagram containers waiting to be published.
    for post in c.db.posts(IG_BUILDING, 10).await? {
        let Some(acct) = c.db.account(&post.account).await? else { continue };
        let Some(tk) = c.db.token(&acct.handle).await? else { continue };
        let user = acct.ig_user_id.clone().unwrap_or_default();
        let container = post.publish_id.clone().unwrap_or_default();
        match ig_publish(&c.ig_api, &tk.access_token, &user, &container).await {
            Ok(Some(id)) => {
                c.db.run("UPDATE posts SET status = ?2, publish_id = ?3 WHERE id = ?1", &[s(&post.id), s(PUBLISHED), s(&id)]).await?
            }
            Ok(None) => {}
            Err((class, msg)) => {
                let state = c.db.account_state(&acct.handle, t, off).await?;
                if class == ErrorClass::Transient {
                    c.db.log(&acct.handle, &post.id, "ig_wait", &msg).await?;
                } else {
                    // The post was never published; the day's slot stays used.
                    let mut st = state;
                    let stopped = policy::on_error(&mut st, class, c.db.stopped().await?);
                    c.db.set_status(&post.id, FAILED, &msg).await?;
                    if matches!(class, ErrorClass::Auth | ErrorClass::SpamRisk) {
                        c.db.set_paused(&acct.handle, true, &msg).await?;
                    }
                    if stopped {
                        c.db.set_setting("stopped", "1").await?;
                    }
                }
            }
        }
    }
    // TikTok status for recent sends.
    for post in c.db.posts(SENT, 10).await? {
        let Some(acct) = c.db.account(&post.account).await? else { continue };
        if acct.platform != "tiktok" {
            continue;
        }
        let Some(tk) = c.db.token(&acct.handle).await? else { continue };
        match tiktok_status(&c.tiktok_api, &tk.access_token, post.publish_id.as_deref().unwrap_or("")).await {
            Some((st, post_id)) if st == "PUBLISH_COMPLETE" => {
                c.db.set_status(&post.id, PUBLISHED, "").await?;
                if let Some(pid) = post_id {
                    c.db.run("UPDATE posts SET post_id = ?2 WHERE id = ?1", &[s(&post.id), s(&pid)]).await?;
                }
            }
            Some((st, _)) if st.starts_with("FAILED") => c.db.set_status(&post.id, FAILED, &format!("TikTok reported {st}")).await?,
            // Keep TikTok's latest status visible, e.g. SEND_TO_USER_INBOX.
            Some((st, _)) => c.db.run("UPDATE posts SET detail = ?2 WHERE id = ?1", &[s(&post.id), s(&st)]).await?,
            None => {}
        }
    }
    // Results, once an hour.
    if t % 3600 < 60 {
        collect_metrics(c, t).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct MetricRow {
    id: String,
    hook: String,
    #[serde(default)]
    caption: String,
    sent_at: Option<f64>,
    post_id: Option<String>,
    views: Option<f64>,
    likes: Option<f64>,
    comments: Option<f64>,
    shares: Option<f64>,
}

/// Read view, like, comment and share counts for the last 14 days of posts.
async fn collect_metrics(c: &Ctx, t: i64) -> Result<()> {
    for (acct, _, _) in c.db.accounts().await? {
        if acct.platform != "tiktok" {
            continue;
        }
        let rows = c
            .db
            .all::<MetricRow>(
                "SELECT id, hook, caption, sent_at, post_id, views, likes, comments, shares FROM posts WHERE account = ?1 AND sent_at > ?2 AND status IN (?3, ?4) ORDER BY sent_at",
                &[s(&acct.handle), n(t - 14 * 86_400), s(SENT), s(PUBLISHED)],
            )
            .await?;
        if rows.is_empty() {
            continue;
        }
        let Some(tk) = c.db.token(&acct.handle).await? else { continue };
        let videos = tiktok_videos(&c.tiktok_api, &tk.access_token).await;
        let mut used = std::collections::HashSet::new();
        let mut events = vec![];
        for r in rows {
            let sent = r.sent_at.unwrap_or(0.0) as i64;
            let Some(v) = match_video(&videos, r.post_id.as_deref(), &r.hook, &r.caption, sent).filter(|v| !used.contains(&v.id)) else { continue };
            used.insert(v.id.clone());
            c.db
                .run(
                    "UPDATE posts SET post_id = ?2, views = ?3, likes = ?4, comments = ?5, shares = ?6, metrics_at = ?7, status = ?8 WHERE id = ?1",
                    &[s(&r.id), s(&v.id), n(v.views), n(v.likes), n(v.comments), n(v.shares), n(t), s(PUBLISHED)],
                )
                .await?;
            let was = |x: Option<f64>| x.unwrap_or(0.0) as i64;
            events.push(ph_event(
                "tiktok_post_stats",
                &format!("relay:{}", acct.handle),
                t,
                json!({
                    "account": acct.handle, "product": acct.product, "post": r.id, "tiktok_id": v.id, "hook": r.hook, "sent_at": sent,
                    "views": v.views, "likes": v.likes, "comments": v.comments, "shares": v.shares,
                    "views_delta": v.views - was(r.views), "likes_delta": v.likes - was(r.likes),
                    "comments_delta": v.comments - was(r.comments), "shares_delta": v.shares - was(r.shares),
                }),
            ));
        }
        ph_send(c, events).await;
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct ResultRow {
    id: String,
    account: String,
    #[serde(default)]
    images: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    sent_at: Option<f64>,
    hook: String,
    status: String,
    views: Option<f64>,
    likes: Option<f64>,
    comments: Option<f64>,
    shares: Option<f64>,
}

fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = xs.len() / 2;
    if xs.len() % 2 == 1 { xs[m] } else { (xs[m - 1] + xs[m]) / 2.0 }
}

/// Posts sent in the last `days` days with their counts. A post is a
/// breakout when its views are at least 5x its account's median.
async fn results(c: &Ctx, days: i64) -> Result<Value> {
    let rows = c
        .db
        .all::<ResultRow>(
            "SELECT id, account, images, comment, sent_at, hook, status, views, likes, comments, shares FROM posts WHERE sent_at > ?1 ORDER BY sent_at DESC LIMIT 500",
            &[n(now() - days * 86_400)],
        )
        .await?;
    let mut by_acct: std::collections::HashMap<String, Vec<f64>> = Default::default();
    for r in &rows {
        if let Some(v) = r.views {
            by_acct.entry(r.account.clone()).or_default().push(v);
        }
    }
    let medians: std::collections::HashMap<String, f64> = by_acct.into_iter().map(|(k, v)| (k, median(v))).collect();
    let posts: Vec<Value> = rows
        .iter()
        .map(|r| {
            let med = medians.get(&r.account).copied().unwrap_or(0.0);
            let breakout = r.views.map(|v| med > 0.0 && v >= 5.0 * med).unwrap_or(false);
            let images: Value = r.images.as_deref().and_then(|i| serde_json::from_str(i).ok()).unwrap_or(json!([]));
            json!({"id": r.id, "account": r.account, "images": images, "comment": r.comment, "sent_at": r.sent_at.map(|x| x as i64), "hook": r.hook, "status": r.status,
                   "views": r.views.map(|x| x as i64), "likes": r.likes.map(|x| x as i64), "comments": r.comments.map(|x| x as i64),
                   "shares": r.shares.map(|x| x as i64), "breakout": breakout})
        })
        .collect();
    Ok(json!({ "days": days, "medians": medians, "posts": posts }))
}

fn results_csv(v: &Value) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut out = String::from("post,account,sent_at_unix,hook,status,views,likes,comments,shares,breakout\n");
    for p in v["posts"].as_array().into_iter().flatten() {
        let num = |k: &str| p[k].as_i64().map(|x| x.to_string()).unwrap_or_default();
        out += &format!(
            "{},{},{},{},{},{},{},{},{},{}\n",
            q(p["id"].as_str().unwrap_or("")),
            q(p["account"].as_str().unwrap_or("")),
            num("sent_at"),
            q(p["hook"].as_str().unwrap_or("")),
            p["status"].as_str().unwrap_or(""),
            num("views"),
            num("likes"),
            num("comments"),
            num("shares"),
            p["breakout"].as_bool().unwrap_or(false)
        );
    }
    out
}

async fn refresh_tokens(c: &Ctx, t: i64) -> Result<()> {
    let (Some(key), Some(secret)) = (&c.tiktok_key, &c.tiktok_secret) else { return Ok(()) };
    for (acct, _, _) in c.db.accounts().await? {
        if acct.platform != "tiktok" {
            continue;
        }
        let Some(tk) = c.db.token(&acct.handle).await? else { continue };
        let (Some(refresh), Some(exp)) = (tk.refresh_token, tk.expires_at) else { continue };
        if exp as i64 - t > 2 * 3600 {
            continue;
        }
        let r = http(
            Method::Post,
            &format!("{}/v2/oauth/token/", c.tiktok_api),
            &[("Content-Type", "application/x-www-form-urlencoded")],
            Some(form(&[("client_key", key), ("client_secret", secret), ("grant_type", "refresh_token"), ("refresh_token", &refresh)])),
        )
        .await?;
        if let Some(access) = r.body["access_token"].as_str() {
            c.db
                .save_token(&acct.handle, access, r.body["refresh_token"].as_str(), r.body["expires_in"].as_i64().map(|e| t + e))
                .await?;
        } else {
            c.db.log(&acct.handle, "", "refresh_failed", &r.text).await?;
        }
    }
    Ok(())
}

#[event(scheduled)]
pub async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    match ctx(&env).await {
        Ok(c) => {
            if let Err(e) = tick(&c).await {
                console_error!("tick error: {e}");
            }
        }
        Err(e) => console_error!("setup error: {e}"),
    }
}

// ---------------------------------------------------------------- auth

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut k = if key.len() > 64 { Sha256::digest(key).to_vec() } else { key.to_vec() };
    k.resize(64, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::digest([ipad.as_slice(), msg].concat());
    Sha256::digest([opad.as_slice(), inner.as_slice()].concat()).to_vec()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Session cookie value: "<expiry>.<hmac(expiry)>", keyed by the admin
/// password, so changing the password signs everyone out.
fn session_cookie(password: &str, exp: i64) -> String {
    format!("{exp}.{}", hex(&hmac(password.as_bytes(), format!("relay-session:{exp}").as_bytes())))
}

fn session_ok(password: &str, value: &str, t: i64) -> bool {
    let Some((exp, _)) = value.split_once('.') else { return false };
    let Ok(exp) = exp.parse::<i64>() else { return false };
    exp > t && constant_eq(session_cookie(password, exp).as_bytes(), value.as_bytes())
}

fn random_hex(len: usize) -> String {
    let mut buf = vec![0u8; len];
    getrandom::getrandom(&mut buf).expect("random");
    hex(&buf)
}

// ---------------------------------------------------------------- http

fn html(body: &str) -> Result<Response> {
    let mut r = Response::from_html(body)?;
    r.headers_mut().set("Cache-Control", "no-store")?;
    Ok(r)
}

fn err(msg: &str, code: u16) -> Result<Response> {
    let mut r = Response::from_json(&json!({ "error": msg }))?.with_status(code);
    r.headers_mut().set("Cache-Control", "no-store")?;
    Ok(r)
}

fn cookie(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get("Cookie")
        .ok()
        .flatten()?
        .split(';')
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(String::from))
}

fn page(title: &str, msg: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>{t}</title><body style=\"font:16px -apple-system,sans-serif;max-width:32rem;margin:4rem auto;padding:0 16px\"><h1 style=\"font-size:20px\">{t}</h1><p>{m}</p>",
        t = esc(title),
        m = esc(msg)
    )
}

#[event(fetch)]
pub async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_string();
    let c = ctx(&env).await?;

    // Public: short link for TikTok bios and slides, credited to TikTok.
    if path == "/tt" || path == "/tt/" {
        return Response::redirect(Url::parse(&format!("{}/?src=tiktok", c.public_url))?);
    }
    // Public: per-account short link, /tt/<account or code>, credited to that
    // account (src=tiktok-<handle>) on the growth dashboard.
    if let Some(code) = path.strip_prefix("/tt/").map(|p| p.trim_end_matches('/')).filter(|p| !p.is_empty()) {
        let handle = match code {
            "sg" => "standard.grow",
            "gfb" => "growthforbusiness",
            "te" => "trade.evidence",
            other => other,
        };
        if handle.len() <= 40 && handle.chars().all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch)) {
            return Response::redirect(Url::parse(&format!("{}/?src=tiktok-{handle}", c.public_url))?);
        }
    }

    // Public: PostHog through our own domain, so ad blockers and PostHog's
    // asset CDN can't silently drop visits from the dashboard.
    if let Some(rest) = path.strip_prefix("/ingest/") {
        let base = if rest.starts_with("static/") { "https://us-assets.i.posthog.com" } else { "https://us.i.posthog.com" };
        let target = format!("{base}/{rest}{}", url.query().map(|q| format!("?{q}")).unwrap_or_default());
        let h = Headers::new();
        for (k, v) in req.headers() {
            if !matches!(k.as_str(), "host" | "cookie") {
                h.set(&k, &v)?;
            }
        }
        if let Some(ip) = req.headers().get("CF-Connecting-IP")? {
            h.set("X-Forwarded-For", &ip)?;
        }
        let method = req.method();
        let mut init = RequestInit::new();
        init.with_method(method.clone()).with_headers(h);
        if !matches!(method, Method::Get | Method::Head) {
            init.with_body(req.inner().body().map(Into::into));
        }
        return Fetch::Request(Request::new_with_init(&target, &init)?).send().await;
    }

    // Public: the homepage. Each visitor gets one hero variant and keeps it
    // (cookie), so the PostHog experiment compares like with like.
    if matches!(path.as_str(), "/" | "/index.html" | "/join" | "/join.html") && req.method() == Method::Get {
        let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
        let valid = |v: &str| matches!(v, "a" | "b" | "c");
        let forced = q.get("hv").filter(|v| valid(v)).cloned();
        let from_cookie = cookie(&req, "hv").filter(|v| valid(v));
        let variant = forced.clone().or(from_cookie.clone()).unwrap_or_else(|| {
            let mut b = [0u8; 1];
            getrandom::getrandom(&mut b).expect("random");
            ["a", "b", "c"][(b[0] % 3) as usize].to_string()
        });
        let asset = format!("{}://{}/h/{variant}", url.scheme(), url.host_str().unwrap_or(""));
        let mut page = env.assets("ASSETS")?.fetch(asset, None).await?;
        let mut html = page.text().await?;
        if let Ok(key) = env.var("POSTHOG_KEY").map(|v| v.to_string()) {
            if !key.is_empty() {
                let host = env.var("POSTHOG_HOST").map(|v| v.to_string()).unwrap_or_else(|_| "https://us.i.posthog.com".into());
                let safe = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || "_-.:/".contains(c));
                if safe(&key) && safe(&host) {
                    html = html.replace("<!--POSTHOG-->", &format!("<meta name=\"posthog-key\" content=\"{key}\"><meta name=\"posthog-host\" content=\"{host}\">"));
                }
            }
        }
        let mut r = Response::from_html(html)?;
        r.headers_mut().set("Cache-Control", "no-store")?;
        if forced.is_none() && from_cookie.is_none() {
            r.headers_mut().set("Set-Cookie", &format!("hv={variant}; Path=/; Max-Age=2592000; SameSite=Lax; Secure"))?;
        }
        return Ok(r);
    }

    // Public: slide images.
    if let Some(key) = path.strip_prefix("/m/") {
        if !media_key_ok(key) {
            return Response::error("not found", 404);
        }
        let kv = env.kv("MEDIA")?;
        return match kv.get(key).bytes().await.map_err(|e| Error::RustError(e.to_string()))? {
            Some(bytes) => {
                let mut r = Response::from_bytes(bytes)?;
                r.headers_mut().set("Content-Type", "image/jpeg")?;
                r.headers_mut().set("Cache-Control", "public, max-age=31536000, immutable")?;
                Ok(r)
            }
            None => Response::error("not found", 404),
        };
    }

    // Public: waitlist sign-up from the landing page.
    if path == "/waitlist" && req.method() == Method::Post {
        let v: Value = req.json().await.unwrap_or(Value::Null);
        if !v["company"].as_str().unwrap_or("").is_empty() {
            // Honeypot filled: a bot. Pretend it worked.
            return Response::from_json(&json!({ "ok": true }));
        }
        let email = v["email"].as_str().unwrap_or("").trim().to_lowercase();
        if !email_ok(&email) {
            return err("Please enter a valid email.", 400);
        }
        let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
        let ip = req.headers().get("CF-Connecting-IP")?.unwrap_or_default();
        let ip_hash = hex(&hmac(b"standard-grow-waitlist", ip.as_bytes()))[..16].to_string();
        let t = now();
        let recent = c
            .db
            .first::<Count>("SELECT COUNT(*) AS n FROM waitlist WHERE ip_hash = ?1 AND created_at > ?2", &[s(&ip_hash), n(t - 3600)])
            .await?
            .map(|c| c.n)
            .unwrap_or(0.0);
        if recent >= 5.0 {
            return err("Too many sign-ups from here. Try again later.", 429);
        }
        let is_new = c.db.first::<Count>("SELECT COUNT(*) AS n FROM waitlist WHERE email = ?1", &[s(&email)]).await?.map(|c| c.n).unwrap_or(0.0) == 0.0;
        if is_new {
            // The visitor's PostHog id ties the sign-up to their visit; the
            // email itself never goes to PostHog.
            let src = clip(v["src"].as_str().unwrap_or(""), 60);
            let (source, variant) = src.split_once('|').unwrap_or((src.as_str(), ""));
            let ph = v["ph"].as_str().unwrap_or("");
            let id = if !ph.is_empty() && ph.len() <= 80 && ph.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_".contains(ch)) {
                ph.to_string()
            } else {
                format!("waitlist:{}", &hex(&hmac(b"standard-grow-waitlist", email.as_bytes()))[..16])
            };
            let props = json!({ "source": if source.is_empty() { "direct" } else { source }, "hero_variant": variant, "has_site": !v["site"].as_str().unwrap_or("").trim().is_empty() });
            ph_send(&c, vec![ph_event("waitlist_joined", &id, t, props)]).await;
        }
        c.db
            .run(
                "INSERT INTO waitlist (email, site, src, created_at, ip_hash) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(email) DO NOTHING",
                &[s(&email), s(&clip(v["site"].as_str().unwrap_or(""), 200)), s(&clip(v["src"].as_str().unwrap_or(""), 60)), n(t), s(&ip_hash)],
            )
            .await?;
        return Response::from_json(&json!({ "ok": true }));
    }

    // Public: "Text me" from the in-app sheet (flagged): a phone number we
    // text first. Stored like the waitlist; the number never goes to PostHog.
    if path == "/api/textme" && req.method() == Method::Post {
        let v: Value = req.json().await.unwrap_or(Value::Null);
        if !v["company"].as_str().unwrap_or("").is_empty() {
            return Response::from_json(&json!({ "ok": true }));
        }
        let digits: String = v["phone"].as_str().unwrap_or("").chars().filter(|c| c.is_ascii_digit()).collect();
        let phone = match digits.len() {
            10 => format!("+1{digits}"),
            11..=15 => format!("+{digits}"),
            _ => return err("Enter a phone number with area code.", 400),
        };
        let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
        let ip = req.headers().get("CF-Connecting-IP")?.unwrap_or_default();
        let ip_hash = hex(&hmac(b"standard-grow-textme", ip.as_bytes()))[..16].to_string();
        let t = now();
        let recent = c
            .db
            .first::<Count>("SELECT COUNT(*) AS n FROM textme WHERE ip_hash = ?1 AND created_at > ?2", &[s(&ip_hash), n(t - 3600)])
            .await?
            .map(|c| c.n)
            .unwrap_or(0.0);
        if recent >= 5.0 {
            return err("Too many requests from here. Try again later.", 429);
        }
        let src = clip(v["src"].as_str().unwrap_or(""), 60);
        let inapp = clip(v["inapp"].as_str().unwrap_or(""), 20);
        let is_new = c.db.first::<Count>("SELECT COUNT(*) AS n FROM textme WHERE phone = ?1", &[s(&phone)]).await?.map(|c| c.n).unwrap_or(0.0) == 0.0;
        if is_new {
            let (source, variant) = src.split_once('|').unwrap_or((src.as_str(), ""));
            let ph = v["ph"].as_str().unwrap_or("");
            let id = if !ph.is_empty() && ph.len() <= 80 && ph.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_".contains(ch)) {
                ph.to_string()
            } else {
                format!("textme:{}", &hex(&hmac(b"standard-grow-textme", phone.as_bytes()))[..16])
            };
            let props = json!({ "source": if source.is_empty() { "direct" } else { source }, "hero_variant": variant, "inapp": inapp });
            ph_send(&c, vec![ph_event("textme_submitted", &id, t, props)]).await;
        }
        c.db
            .run(
                "INSERT INTO textme (phone, src, inapp, created_at, ip_hash) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(phone) DO NOTHING",
                &[s(&phone), s(&src), s(&inapp), n(t), s(&ip_hash)],
            )
            .await?;
        return Response::from_json(&json!({ "ok": true }));
    }

    // Public: the optional follow-up after sign-up (two taps).
    if path == "/waitlist/profile" && req.method() == Method::Post {
        let v: Value = req.json().await.unwrap_or(Value::Null);
        let email = v["email"].as_str().unwrap_or("").trim().to_lowercase();
        let business = v["business"].as_str().unwrap_or("");
        let community = v["community"].as_str().unwrap_or("");
        if !BUSINESS_TYPES.contains(&business) && !business.is_empty() || !COMMUNITY_APPS.contains(&community) && !community.is_empty() {
            return err("Pick one of the options.", 400);
        }
        // Only the browser that just signed up can add to its row.
        let ip = req.headers().get("CF-Connecting-IP")?.unwrap_or_default();
        let ip_hash = hex(&hmac(b"standard-grow-waitlist", ip.as_bytes()))[..16].to_string();
        c.db
            .run(
                "UPDATE waitlist SET business = COALESCE(NULLIF(?2, ''), business), community = COALESCE(NULLIF(?3, ''), community)
                 WHERE email = ?1 AND ip_hash = ?4 AND created_at > ?5",
                &[s(&email), s(business), s(community), s(&ip_hash), n(now() - 3600)],
            )
            .await?;
        return Response::from_json(&json!({ "ok": true }));
    }

    // Public: TikTok sign-in callback.
    if path == "/callback" {
        let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
        if let Some(e) = q.get("error") {
            return html(&page("Sign-in cancelled", e));
        }
        let (Some(state), Some(code)) = (q.get("state"), q.get("code")) else { return html(&page("Sign-in failed", "missing code")) };
        #[derive(Deserialize)]
        struct St {
            handle: String,
            created_at: f64,
        }
        let Some(st) = c.db.first::<St>("SELECT handle, created_at FROM oauth_states WHERE state = ?1", &[s(state)]).await? else {
            return html(&page("Sign-in failed", "This sign-in link expired. Start again from the setup page."));
        };
        c.db.run("DELETE FROM oauth_states WHERE state = ?1", &[s(state)]).await?;
        if now() - st.created_at as i64 > 1800 {
            return html(&page("Sign-in failed", "This sign-in link expired. Start again from the setup page."));
        }
        let (Some(key), Some(secret)) = (&c.tiktok_key, &c.tiktok_secret) else {
            return html(&page("Sign-in failed", "Add the TikTok client key and secret on the setup page first."));
        };
        let r = http(
            Method::Post,
            &format!("{}/v2/oauth/token/", c.tiktok_api),
            &[("Content-Type", "application/x-www-form-urlencoded")],
            Some(form(&[
                ("client_key", key),
                ("client_secret", secret),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", &format!("{}/callback", c.public_url)),
            ])),
        )
        .await?;
        let Some(access) = r.body["access_token"].as_str() else {
            return html(&page("Sign-in failed", &format!("TikTok said: {}", r.text)));
        };
        // Refuse a sign-in to a different TikTok account than the one being
        // connected; otherwise that account's posts land on the wrong profile.
        if let Ok(info) = tiktok_creator_info(&c.tiktok_api, access).await {
            let got = info["creator_username"].as_str().unwrap_or("");
            if !got.is_empty() && !got.eq_ignore_ascii_case(&st.handle) {
                return html(&page(
                    "Wrong TikTok account",
                    &format!("You signed in as @{got}, not @{}. Switch to @{} in TikTok (or log out on tiktok.com), then press Connect again.", st.handle, st.handle),
                ));
            }
        }
        c.db
            .save_token(&st.handle, access, r.body["refresh_token"].as_str(), r.body["expires_in"].as_i64().map(|e| now() + e))
            .await?;
        c.db.set_paused(&st.handle, false, "").await?;
        return html(&page("Connected", &format!("@{} is connected. You can close this tab.", st.handle)));
    }

    if !(path == "/admin" || path.starts_with("/admin/")) {
        // Everything else is the static site (served before the Worker runs).
        return Response::error("not found", 404);
    }

    // Setup: password protected.
    let password = env.secret("ADMIN_PASSWORD").map(|v| v.to_string()).unwrap_or_default();
    if password.chars().count() < 12 {
        return Response::error("Set ADMIN_PASSWORD (12+ characters) with `wrangler secret put ADMIN_PASSWORD`.", 503);
    }
    let method = req.method();
    if method != Method::Get && req.headers().get("X-Relay")?.as_deref() != Some("1") {
        return err("missing X-Relay header", 403);
    }
    let t = now();
    if path == "/admin/api/login" && method == Method::Post {
        let minute = (t / 60).to_string();
        let fails = c.db.setting(&format!("login_fail:{minute}")).await?.and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        if fails >= 10 {
            return err("too many attempts; wait a minute", 429);
        }
        let v: Value = req.json().await.unwrap_or(Value::Null);
        if !constant_eq(v["password"].as_str().unwrap_or("").as_bytes(), password.as_bytes()) {
            c.db.set_setting(&format!("login_fail:{minute}"), &(fails + 1).to_string()).await?;
            return err("wrong password", 401);
        }
        let exp = t + 12 * 3600;
        let mut r = Response::from_json(&json!({}))?;
        r.headers_mut().set(
            "Set-Cookie",
            &format!("relay_session={}; Path=/admin; HttpOnly; Secure; SameSite=Strict; Max-Age=43200", session_cookie(&password, exp)),
        )?;
        return Ok(r);
    }
    let session = cookie(&req, "relay_session").or_else(|| req.headers().get("X-Relay-Session").ok().flatten());
    if !session.map(|v| session_ok(&password, &v, t)).unwrap_or(false) {
        return if path == "/admin" || path == "/admin/" { html(LOGIN_PAGE) } else { err("sign in first", 401) };
    }

    let result: std::result::Result<Value, String> = match (method, path.as_str()) {
        (Method::Get, "/admin") | (Method::Get, "/admin/") => return html(SETUP_PAGE),
        (Method::Get, "/admin/api/state") => state(&c).await.map_err(|e| e.to_string()),
        (Method::Get, "/admin/api/textme.csv") => {
            let mut r = Response::ok(textme_csv(&c).await?)?;
            r.headers_mut().set("Content-Type", "text/csv; charset=utf-8")?;
            r.headers_mut().set("Content-Disposition", "attachment; filename=\"standard-grow-textme.csv\"")?;
            return Ok(r);
        }
        (Method::Get, "/admin/api/waitlist.csv") => {
            let mut r = Response::ok(waitlist_csv(&c).await?)?;
            r.headers_mut().set("Content-Type", "text/csv; charset=utf-8")?;
            r.headers_mut().set("Content-Disposition", "attachment; filename=\"standard-grow-waitlist.csv\"")?;
            return Ok(r);
        }
        (Method::Get, "/admin/api/results") | (Method::Get, "/admin/api/results.csv") => {
            let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
            let days = q.get("days").and_then(|d| d.parse::<i64>().ok()).unwrap_or(7).clamp(1, 90);
            let v = results(&c, days).await?;
            if path.ends_with(".csv") {
                let mut r = Response::ok(results_csv(&v))?;
                r.headers_mut().set("Content-Type", "text/csv; charset=utf-8")?;
                r.headers_mut().set("Content-Disposition", &format!("attachment; filename=\"standard-grow-results-{days}d.csv\""))?;
                return Ok(r);
            }
            Ok(v)
        }
        (Method::Get, "/admin/api/login-url") => {
            let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
            login_url(&c, q.get("handle").map(String::as_str).unwrap_or("")).await.map(|u| json!({ "url": u }))
        }
        (Method::Get, "/admin/api/creator-info") => {
            let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
            let h = q.get("handle").cloned().unwrap_or_default();
            match c.db.token(&h).await.map_err(|e| e.to_string())? {
                Some(tk) => tiktok_creator_info(&c.tiktok_api, &tk.access_token).await,
                None => Err(format!("@{h} is not connected")),
            }
        }
        (Method::Post, "/admin/api/approve") => {
            let v: Value = req.json().await.unwrap_or(Value::Null);
            approve(&c, &v).await
        }
        (Method::Post, "/admin/api/settings") => {
            let v: Value = req.json().await.unwrap_or(Value::Null);
            save_settings(&c, &v).await
        }
        (Method::Post, "/admin/api/account") => {
            let v: Value = req.json().await.unwrap_or(Value::Null);
            upsert_account(&c, v).await
        }
        (Method::Post, "/admin/api/account/delete") => {
            let v: Value = req.json().await.unwrap_or(Value::Null);
            let h = v["handle"].as_str().unwrap_or("").to_string();
            c.db.run("DELETE FROM accounts WHERE handle = ?1", &[s(&h)]).await.map_err(|e| e.to_string())?;
            c.db.run("DELETE FROM tokens WHERE handle = ?1", &[s(&h)]).await.map_err(|e| e.to_string())?;
            state(&c).await.map_err(|e| e.to_string())
        }
        (Method::Post, "/admin/api/action") => {
            let v: Value = req.json().await.unwrap_or(Value::Null);
            action(&c, &v).await
        }
        (Method::Put, p) if p.starts_with("/admin/api/media/") => {
            let key = &p["/admin/api/media/".len()..];
            if !media_key_ok(key) {
                return err("bad media key", 400);
            }
            let bytes = req.bytes().await?;
            if bytes.len() < 4 || bytes[..2] != [0xFF, 0xD8] || bytes.len() > 8 << 20 {
                return err("media must be a JPEG under 8 MB", 400);
            }
            // Kept 14 days: long enough to be scheduled ahead and fetched,
            // short enough to stay inside the free KV storage.
            env.kv("MEDIA")?
                .put_bytes(key, &bytes)
                .map_err(|e| Error::RustError(e.to_string()))?
                .expiration_ttl(14 * 24 * 3600)
                .execute()
                .await
                .map_err(|e| Error::RustError(e.to_string()))?;
            Ok(json!({ "key": key }))
        }
        (Method::Post, "/admin/api/import") => {
            let v: Value = req.json().await.unwrap_or(Value::Null);
            import(&c, &env, &v).await
        }
        _ => return err("not found", 404),
    };
    match result {
        Ok(v) => {
            let mut r = Response::from_json(&v)?;
            r.headers_mut().set("Cache-Control", "no-store")?;
            Ok(r)
        }
        Err(e) => err(&e, 400),
    }
}

const BUSINESS_TYPES: &[&str] = &["store", "app", "newsletter", "creator", "service", "other"];
const COMMUNITY_APPS: &[&str] = &["discord", "slack", "telegram", "whatsapp", "none"];

/// A plain sanity check, not full RFC 5322: one @, a dot in the domain,
/// no spaces, sensible length.
fn email_ok(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else { return false };
    e.len() <= 254
        && !local.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !e.chars().any(|c| c.is_whitespace() || c == '<' || c == '>' || c == '"')
}

/// Media keys are content hashes: 24 hex characters + ".jpg".
fn media_key_ok(key: &str) -> bool {
    key.len() == 28 && key.ends_with(".jpg") && key[..24].chars().all(|c| c.is_ascii_hexdigit())
}

async fn state(c: &Ctx) -> Result<Value> {
    let t = now();
    let off = c.db.utc_offset().await?;
    let mut accounts = vec![];
    for (a, _, _) in c.db.accounts().await? {
        let st = c.db.account_state(&a.handle, t, off).await?;
        let connected = a.platform == "dryrun" || c.db.token(&a.handle).await?.is_some();
        accounts.push(json!({
            "config": a, "connected": connected, "paused": st.paused, "today": st.posted_today,
            "cap_today": policy::effective_cap(&a.limits(), st.started_at, t, off), "pending": st.pending,
        }));
    }
    // Read these fresh: they may have just been saved in this request.
    let key = c.db.setting("tiktok_client_key").await?.unwrap_or_default();
    let secret_set = c.tiktok_secret.is_some() || c.db.setting("tiktok_client_secret").await?.map(|s| !s.is_empty()).unwrap_or(false);
    let queue: Vec<Value> = c
        .db
        .posts(QUEUED, 50)
        .await?
        .into_iter()
        .map(|p| {
            let images: Value = serde_json::from_str(&p.images).unwrap_or(json!([]));
            json!({"id": p.id, "account": p.account, "at": p.scheduled_at as i64, "hook": p.hook, "caption": p.caption, "images": images, "comment": p.comment, "approved": p.settings.is_some()})
        })
        .collect();
    let mut attention = vec![];
    for st in [UNKNOWN, FAILED] {
        for p in c.db.posts(st, 20).await? {
            attention.push(json!({"id": p.id, "account": p.account, "status": st, "hook": p.hook}));
        }
    }
    Ok(json!({
        "settings": {
            "utc_offset_hours": off / 3600,
            "media_base_url": c.public_url,
            "jitter_minutes": c.db.setting("jitter_minutes").await?.and_then(|v| v.parse::<i64>().ok()).unwrap_or(7),
            "tiktok_client_key": key,
            "tiktok_secret_set": secret_set,
            "redirect_uri": format!("{}/callback", c.public_url),
            "hosted": true,
        },
        "stopped": c.db.stopped().await?,
        "waitlist": waitlist_summary(c).await?,
        "textme": c.db.first::<Count>("SELECT COUNT(*) AS n FROM textme", &[]).await.ok().flatten().map(|c| c.n as i64).unwrap_or(0),
        "accounts": accounts, "queue": queue, "attention": attention, "now": t,
    }))
}

#[derive(Deserialize)]
struct SrcCount {
    src: Option<String>,
    n: f64,
}

async fn waitlist_summary(c: &Ctx) -> Result<Value> {
    let total = c.db.first::<Count>("SELECT COUNT(*) AS n FROM waitlist", &[]).await?.map(|c| c.n as i64).unwrap_or(0);
    let by_src = c.db.all::<SrcCount>("SELECT src, COUNT(*) AS n FROM waitlist GROUP BY src ORDER BY n DESC", &[]).await?;
    let by: serde_json::Map<String, Value> =
        by_src.into_iter().map(|r| (r.src.filter(|s| !s.is_empty()).unwrap_or_else(|| "direct".into()), json!(r.n as i64))).collect();
    let count_by = |col: &'static str| async move {
        let rows = c.db.all::<SrcCount>(&format!("SELECT {col} AS src, COUNT(*) AS n FROM waitlist WHERE {col} IS NOT NULL GROUP BY {col} ORDER BY n DESC"), &[]).await?;
        Ok::<serde_json::Map<String, Value>, Error>(rows.into_iter().map(|r| (r.src.unwrap_or_default(), json!(r.n as i64))).collect())
    };
    let business = count_by("business").await?;
    let community = count_by("community").await?;
    Ok(json!({ "total": total, "by_source": by, "by_business": business, "by_community": community }))
}

#[derive(Deserialize)]
struct TextmeRow {
    phone: String,
    src: Option<String>,
    inapp: Option<String>,
    created_at: f64,
}

/// "Text me" requests, newest first: the numbers to text back.
async fn textme_csv(c: &Ctx) -> Result<String> {
    let rows = c.db.all::<TextmeRow>("SELECT phone, src, inapp, created_at FROM textme ORDER BY created_at DESC", &[]).await?;
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut out = String::from("phone,source,in_app_browser,requested_unix\n");
    for r in rows {
        out += &format!("{},{},{},{}\n", q(&r.phone), q(r.src.as_deref().unwrap_or("")), q(r.inapp.as_deref().unwrap_or("")), r.created_at as i64);
    }
    Ok(out)
}

#[derive(Deserialize)]
struct WaitRow {
    email: String,
    site: Option<String>,
    src: Option<String>,
    created_at: f64,
    business: Option<String>,
    community: Option<String>,
}

async fn waitlist_csv(c: &Ctx) -> Result<String> {
    let rows = c.db.all::<WaitRow>("SELECT email, site, src, created_at, business, community FROM waitlist ORDER BY created_at", &[]).await?;
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut out = String::from("email,website_or_post,source,signed_up_unix,business,community_app\n");
    for r in rows {
        out += &format!(
            "{},{},{},{},{},{}\n",
            q(&r.email),
            q(r.site.as_deref().unwrap_or("")),
            q(r.src.as_deref().unwrap_or("")),
            r.created_at as i64,
            r.business.as_deref().unwrap_or(""),
            r.community.as_deref().unwrap_or("")
        );
    }
    Ok(out)
}

async fn save_settings(c: &Ctx, v: &Value) -> std::result::Result<Value, String> {
    let e = |e: Error| e.to_string();
    if let Some(h) = v["utc_offset_hours"].as_i64() {
        if !(-12..=14).contains(&h) {
            return Err("UTC offset must be between -12 and 14".into());
        }
        c.db.set_setting("utc_offset_hours", &h.to_string()).await.map_err(e)?;
    }
    if let Some(j) = v["jitter_minutes"].as_i64() {
        c.db.set_setting("jitter_minutes", &j.clamp(0, 30).to_string()).await.map_err(e)?;
    }
    if let Some(k) = v["tiktok_client_key"].as_str().filter(|s| !s.is_empty()) {
        c.db.set_setting("tiktok_client_key", k.trim()).await.map_err(e)?;
    }
    if let Some(k) = v["tiktok_client_secret"].as_str().filter(|s| !s.is_empty()) {
        c.db.set_setting("tiktok_client_secret", k.trim()).await.map_err(e)?;
    }
    // Re-read so the reply shows what was saved.
    state(c).await.map_err(e)
}

/// Record the creator's consent and settings for one queued post. Checked
/// against fresh creator info so a stale option can't be sent.
async fn approve(c: &Ctx, v: &Value) -> std::result::Result<Value, String> {
    let e = |e: Error| e.to_string();
    let id = v["id"].as_str().ok_or("post id missing")?;
    let post = c
        .db
        .first::<PostRow>(
            "SELECT id, account, scheduled_at, images, caption, hook, publish_id, mode, comment, settings FROM posts WHERE id = ?1 AND status = ?2",
            &[s(id), s(QUEUED)],
        )
        .await
        .map_err(e)?
        .ok_or("that post is no longer waiting")?;
    let tk = c.db.token(&post.account).await.map_err(e)?.ok_or("account not connected")?;
    let info = tiktok_creator_info(&c.tiktok_api, &tk.access_token).await?;
    let privacy = v["privacy_level"].as_str().filter(|p| !p.is_empty()).ok_or("choose who can view this post")?;
    let allowed = info["privacy_level_options"].as_array().map(|a| a.iter().any(|x| x.as_str() == Some(privacy))).unwrap_or(false);
    if !allowed {
        return Err("that privacy option isn't available for this account".into());
    }
    let branded = v["brand_content_toggle"].as_bool().unwrap_or(false);
    if branded && privacy == "SELF_ONLY" {
        return Err("branded content can't be private".into());
    }
    let disable_comment = v["disable_comment"].as_bool().unwrap_or(false) || info["comment_disabled"].as_bool().unwrap_or(false);
    let settings = json!({
        "privacy_level": privacy,
        "disable_comment": disable_comment,
        "brand_content_toggle": branded,
        "brand_organic_toggle": v["brand_organic_toggle"].as_bool().unwrap_or(false),
    });
    let caption = v["caption"].as_str().unwrap_or(&post.caption).chars().take(2000).collect::<String>();
    c.db
        .run(
            "UPDATE posts SET settings = ?2, approved_at = ?3, caption = ?4 WHERE id = ?1",
            &[s(id), s(&settings.to_string()), n(now()), s(&caption)],
        )
        .await
        .map_err(e)?;
    state(c).await.map_err(e)
}

async fn upsert_account(c: &Ctx, v: Value) -> std::result::Result<Value, String> {
    let a: AccountConfig = serde_json::from_value(v).map_err(|e| format!("account fields: {e}"))?;
    a.validate()?;
    c.db
        .run(
            "INSERT INTO accounts (handle, config) VALUES (?1, ?2) ON CONFLICT(handle) DO UPDATE SET config = ?2",
            &[s(&a.handle), s(&serde_json::to_string(&a).unwrap())],
        )
        .await
        .map_err(|e| e.to_string())?;
    state(c).await.map_err(|e| e.to_string())
}

async fn login_url(c: &Ctx, handle: &str) -> std::result::Result<String, String> {
    c.db.account(handle).await.map_err(|e| e.to_string())?.ok_or(format!("no account {handle}"))?;
    let key = c.tiktok_key.clone().ok_or("add your TikTok client key in step 1 first")?;
    let state = format!("{handle}.{}", random_hex(12));
    c.db
        .run("INSERT INTO oauth_states (state, handle, created_at) VALUES (?1, ?2, ?3)", &[s(&state), s(handle), n(now())])
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "https://www.tiktok.com/v2/auth/authorize/?client_key={}&scope={}&response_type=code&redirect_uri={}&state={}",
        enc(&key),
        enc("user.info.basic,video.upload,video.publish,video.list"),
        enc(&format!("{}/callback", c.public_url)),
        enc(&state)
    ))
}

async fn action(c: &Ctx, v: &Value) -> std::result::Result<Value, String> {
    let e = |e: Error| e.to_string();
    let handle = v["handle"].as_str().unwrap_or("");
    match v["do"].as_str().unwrap_or("") {
        "pause" => c.db.set_paused(handle, true, "paused from setup page").await.map_err(e)?,
        "resume" => c.db.set_paused(handle, false, "").await.map_err(e)?,
        "stop" => c.db.set_setting("stopped", "1").await.map_err(e)?,
        "start" => c.db.set_setting("stopped", "0").await.map_err(e)?,
        "token" => {
            let tok = v["token"].as_str().unwrap_or("").trim();
            if tok.is_empty() {
                return Err("paste the token first".into());
            }
            c.db.save_token(handle, tok, None, None).await.map_err(e)?
        }
        "requeue" => c.db.requeue(v["id"].as_str().unwrap_or(""), now()).await.map_err(e)?,
        "drop" => c.db.set_status(v["id"].as_str().unwrap_or(""), DROPPED, "dropped from setup page").await.map_err(e)?,
        other => return Err(format!("unknown action {other}")),
    }
    state(c).await.map_err(e)
}

/// Queue a day pushed by `relay push`: {date, posts: [{post, account, slot,
/// caption, hook, images: [media keys already uploaded]}]}.
async fn import(c: &Ctx, env: &Env, v: &Value) -> std::result::Result<Value, String> {
    let date = v["date"].as_str().ok_or("date missing")?;
    schedule::parse_date(date).map_err(|e| e.to_string())?;
    let off = c.db.utc_offset().await.map_err(|e| e.to_string())?;
    let jitter = c.db.setting("jitter_minutes").await.map_err(|e| e.to_string())?.and_then(|v| v.parse::<i64>().ok()).unwrap_or(7);
    let kv = env.kv("MEDIA").map_err(|e| e.to_string())?;
    let posts = v["posts"].as_array().ok_or("posts missing")?;
    // Check everything before writing anything.
    for p in posts {
        let post = p["post"].as_str().unwrap_or("");
        if post.is_empty() || !post.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) || post.contains("..") {
            return Err(format!("bad post name {post:?}"));
        }
        let acct = p["account"].as_str().unwrap_or("");
        if c.db.account(acct).await.map_err(|e| e.to_string())?.is_none() {
            return Err(format!("{post}: account {acct} is not set up"));
        }
        schedule::parse_slot(p["slot"].as_str().unwrap_or("")).map_err(|e| format!("{post}: {e}"))?;
        let imgs = p["images"].as_array().ok_or(format!("{post}: images missing"))?;
        if imgs.is_empty() {
            return Err(format!("{post}: no images"));
        }
        for k in imgs {
            let k = k.as_str().unwrap_or("");
            if !media_key_ok(k) || kv.get(k).bytes().await.map_err(|e| e.to_string())?.is_none() {
                return Err(format!("{post}: image {k} was not uploaded"));
            }
        }
    }
    let mut added = 0;
    for p in posts {
        let id = format!("{date}/{}", p["post"].as_str().unwrap_or(""));
        let at = schedule::slot_time(date, p["slot"].as_str().unwrap_or(""), off).map_err(|e| e.to_string())?
            + schedule::jitter_secs(&id, jitter);
        let before = c.db.first::<Count>("SELECT COUNT(*) AS n FROM posts WHERE id = ?1", &[s(&id)]).await.map_err(|e| e.to_string())?;
        if before.map(|c| c.n > 0.0).unwrap_or(false) {
            continue;
        }
        c.db
            .run(
                "INSERT INTO posts (id, account, scheduled_at, images, caption, hook, status, comment) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                &[
                    s(&id),
                    s(p["account"].as_str().unwrap_or("")),
                    n(at),
                    s(&p["images"].to_string()),
                    s(p["caption"].as_str().unwrap_or("")),
                    s(p["hook"].as_str().unwrap_or("")),
                    s(QUEUED),
                    s(p["comment"].as_str().unwrap_or("")),
                ],
            )
            .await
            .map_err(|e| e.to_string())?;
        // Starts the account's ramp on its first queued post.
        c.db
            .run("UPDATE accounts SET started_at = COALESCE(started_at, ?2) WHERE handle = ?1", &[s(p["account"].as_str().unwrap_or("")), n(now())])
            .await
            .map_err(|e| e.to_string())?;
        added += 1;
    }
    Ok(json!({ "queued": added }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        assert_eq!(
            hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn sessions_expire_and_cannot_be_forged() {
        let c = session_cookie("a long admin password", 2_000);
        assert!(session_ok("a long admin password", &c, 1_000));
        assert!(!session_ok("a long admin password", &c, 2_001));
        assert!(!session_ok("another long password", &c, 1_000));
        let forged = format!("9999999999.{}", &c[5..]);
        assert!(!session_ok("a long admin password", &forged, 1_000));
    }

    fn video(id: &str, title: &str, create_time: i64, views: i64) -> VideoStats {
        VideoStats { id: id.into(), title: title.into(), description: String::new(), create_time, views, likes: 0, comments: 0, shares: 0 }
    }

    #[test]
    fn matches_videos_by_id_then_by_hook_and_time() {
        let vs = vec![
            video("1", "7 Realizations That Will Make You Better", 1_000_500, 900),
            video("2", "Something else", 1_000_100, 50),
            video("3", "7 realizations that will make you better", 900_000, 10),
        ];
        assert_eq!(match_video(&vs, Some("2"), "x", "", 0).unwrap().id, "2");
        // By hook: case-insensitive, after the send, not an older repost.
        assert_eq!(match_video(&vs, None, "7 Realizations That Will Make You Better at Golf", "", 1_000_000).unwrap().id, "1");
        assert!(match_video(&vs, None, "Never posted", "", 1_000_000).is_none());
        assert!(match_video(&vs, None, "7 Realizations That Will", "", 2_000_000).is_none(), "too long after the send");
    }

    #[test]
    fn parses_video_list() {
        let body = json!({"data": {"videos": [{"id": "7300", "title": "t", "create_time": 5, "view_count": 12, "like_count": 3}]}});
        let v = parse_videos(&body);
        assert_eq!(v[0].id, "7300");
        assert_eq!((v[0].views, v[0].likes, v[0].shares), (12, 3, 0));
    }

    #[test]
    fn medians_and_csv() {
        assert_eq!(median(vec![1.0, 9.0, 3.0]), 3.0);
        assert_eq!(median(vec![1.0, 3.0]), 2.0);
        let v = json!({"posts": [{"id": "d/a-1", "account": "a", "sent_at": 5, "hook": "say \"hi\"", "status": "published", "views": 10, "breakout": true}]});
        assert!(results_csv(&v).contains("\"say \"\"hi\"\"\""));
    }

    #[test]
    fn emails() {
        for ok in ["a@b.co", "andy+grow@standardtrade.ai"] {
            assert!(email_ok(ok), "{ok}");
        }
        for bad in ["", "a", "a@b", "@b.co", "a@.co", "a@b.", "a b@c.co", "a@b@c.co", "<a@b.co>"] {
            assert!(!email_ok(bad), "{bad}");
        }
    }

    #[test]
    fn media_keys() {
        assert!(media_key_ok("0123456789abcdef01234567.jpg"));
        assert!(!media_key_ok("../etc/passwd"));
        assert!(!media_key_ok("0123456789abcdef0123456z.jpg"));
    }

    #[test]
    fn error_classes_match_native() {
        assert_eq!(tiktok_class("spam_risk_too_many_posts", 200), ErrorClass::SpamRisk);
        assert_eq!(tiktok_class("", 429), ErrorClass::SpamRisk);
        assert_eq!(tiktok_class("access_token_invalid", 401), ErrorClass::Auth);
        assert_eq!(meta_class(368, 0), ErrorClass::SpamRisk);
        assert_eq!(meta_class(0, 2_207_026), ErrorClass::BadPost);
    }
}
