//! End-to-end tests: real SQLite, real HTTP against a fake TikTok and a fake
//! Instagram, the real engine.

use std::sync::{Arc, Mutex};

use relay::config::Config;
use relay::engine::{tick, Providers};
use relay::manifest;
use relay::providers::{instagram::Instagram, tiktok::TikTok, DryRun};
use relay::store::{self, Store};
use serde_json::{json, Value};

/// A fake platform: answers each request from a script, records requests.
struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

fn fake(responder: impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static) -> Fake {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).unwrap();
            let path = req.url().to_string();
            let n = {
                let mut l = log.lock().unwrap();
                l.push((path.clone(), body.clone()));
                l.len()
            };
            let (code, resp) = responder(&path, &body, n);
            let _ = req.respond(tiny_http::Response::from_string(resp).with_status_code(code));
        }
    });
    Fake { url, seen }
}

struct World {
    _dir: tempfile::TempDir,
    cfg: Config,
    store: Store,
    manifest: String,
}

/// Day used by the tests: 2026-10-26, UTC-7.
const DATE: &str = "2026-10-26";
fn at(hh: i64, mm: i64) -> i64 {
    relay::schedule::slot_time(DATE, &format!("{hh:02}:{mm:02}"), -7 * 3600).unwrap()
}

fn world(accounts: Value, posts: &[(&str, &str, &str)]) -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut entries = vec![];
    for (i, (post, account, slot)) in posts.iter().enumerate() {
        let mut imgs = vec![];
        for s in 0..3 {
            let p = dir.path().join(format!("{post}-{s}.png"));
            image::RgbImage::from_pixel(4, 4, image::Rgb([i as u8, s as u8, 9])).save(&p).unwrap();
            imgs.push(p.to_string_lossy().to_string());
        }
        entries.push(json!({"post": post, "account": account, "slot": slot, "images": imgs, "caption": "cap", "hook": format!("hook {i}")}));
    }
    let manifest = serde_json::to_string(&entries).unwrap();
    let cfg: Config = serde_json::from_value(json!({
        "utc_offset_hours": -7,
        "media_dir": dir.path().join("media").to_string_lossy(),
        "media_base_url": "https://media.example.com",
        "jitter_minutes": 0,
        "accounts": accounts,
    }))
    .unwrap();
    cfg.validate().unwrap();
    let store = Store::open(dir.path().join("relay.db").to_str().unwrap()).unwrap();
    World { _dir: dir, cfg, store, manifest }
}

fn tiktok_account(handle: &str, cap: u32, gap: i64) -> Value {
    json!({"handle": handle, "platform": "tiktok", "mode": "inbox", "daily_cap": cap, "ramp": [cap, cap, cap, cap], "min_gap_minutes": gap})
}

fn ok_tiktok() -> Fake {
    fake(|path, _, n| {
        if path.contains("status") {
            (200, json!({"data": {"status": "SEND_TO_USER_INBOX"}, "error": {"code": "ok"}}).to_string())
        } else {
            (200, json!({"data": {"publish_id": format!("p{n}")}, "error": {"code": "ok"}}).to_string())
        }
    })
}

fn run(w: &World, tt: &TikTok, ig: &Instagram, now: i64) -> relay::engine::TickReport {
    let dr = DryRun;
    tick(&w.cfg, &w.store, &Providers { tiktok: tt, instagram: ig, dryrun: &dr }, now).unwrap()
}

fn ig_none() -> Instagram {
    Instagram { api_base: "http://127.0.0.1:9".into(), poll_secs: 0, max_polls: 1 }
}

#[test]
fn posts_a_slideshow_to_tiktok_inbox() {
    let f = ok_tiktok();
    let w = world(json!([tiktok_account("a", 5, 60)]), &[("a-1", "a", "07:00")]);
    w.store.save_token("a", "tok", None, None).unwrap();
    assert_eq!(manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap(), 1);
    // Re-importing is a no-op.
    assert_eq!(manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap(), 0);

    let tt = TikTok { api_base: f.url.clone() };
    assert_eq!(run(&w, &tt, &ig_none(), at(6, 59)).sent, 0, "nothing before its slot");
    let r = run(&w, &tt, &ig_none(), at(7, 0));
    assert_eq!(r.sent, 1);

    let seen = f.seen.lock().unwrap();
    let (path, body) = &seen[0];
    assert_eq!(path, "/v2/post/publish/content/init/");
    let b: Value = serde_json::from_str(body).unwrap();
    assert_eq!(b["post_mode"], "MEDIA_UPLOAD");
    assert_eq!(b["media_type"], "PHOTO");
    let urls = b["source_info"]["photo_images"].as_array().unwrap();
    assert_eq!(urls.len(), 3);
    for u in urls {
        let u = u.as_str().unwrap();
        assert!(u.starts_with("https://media.example.com/") && u.ends_with(".jpg"));
        let name = u.rsplit('/').next().unwrap();
        assert!(relay::media::resolve(std::path::Path::new(&w.cfg.media_dir), name).is_some());
    }
    assert_eq!(w.store.by_status(store::SENT).unwrap().len(), 1);
}

#[test]
fn never_exceeds_daily_cap_or_gap() {
    let f = ok_tiktok();
    let posts: Vec<(String, &str, String)> = (0..8).map(|i| (format!("a-{i}"), "a", format!("07:{:02}", i))).collect();
    let refs: Vec<(&str, &str, &str)> = posts.iter().map(|(p, a, s)| (p.as_str(), *a, s.as_str())).collect();
    let w = world(json!([tiktok_account("a", 3, 60)]), &refs);
    w.store.save_token("a", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    let mut sent = 0;
    let mut times = vec![];
    // Tick every 5 minutes for the whole local day.
    let mut t = at(0, 0);
    while t < at(23, 59) {
        let r = run(&w, &tt, &ig_none(), t);
        if r.sent > 0 {
            times.push(t);
        }
        sent += r.sent;
        t += 300;
    }
    assert_eq!(sent, 3, "cap is 3 a day");
    for pair in times.windows(2) {
        assert!(pair[1] - pair[0] >= 3600, "posts at least 60 minutes apart");
    }
}

#[test]
fn inbox_mode_never_has_more_than_five_pending() {
    let f = ok_tiktok();
    let posts: Vec<(String, &str, String)> = (0..9).map(|i| (format!("a-{i}"), "a", "07:00".to_string())).collect();
    let refs: Vec<(&str, &str, &str)> = posts.iter().map(|(p, a, s)| (p.as_str(), *a, s.as_str())).collect();
    let w = world(json!([tiktok_account("a", 10, 0)]), &refs);
    w.store.save_token("a", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    let mut sent = 0;
    for m in 0..60 {
        sent += run(&w, &tt, &ig_none(), at(7, m)).sent;
    }
    assert_eq!(sent, 5, "drafts stay in the inbox, so only 5 can be pending");
}

#[test]
fn spam_risk_stops_everything_and_pauses_the_account() {
    let f = fake(|_, _, _| (200, json!({"error": {"code": "spam_risk_too_many_posts", "message": "slow down"}}).to_string()));
    let w = world(
        json!([tiktok_account("a", 5, 0), tiktok_account("b", 5, 0)]),
        &[("a-1", "a", "07:00"), ("b-1", "b", "07:01")],
    );
    w.store.save_token("a", "tok", None, None).unwrap();
    w.store.save_token("b", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    let r = run(&w, &tt, &ig_none(), at(7, 5));
    assert!(r.stopped);
    assert_eq!(r.sent, 0);
    assert!(w.store.stopped().unwrap());
    assert!(w.store.account_state("a", at(7, 5), -7 * 3600).unwrap().paused);
    // Account b was never tried, and nothing goes out later either.
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    assert_eq!(run(&w, &tt, &ig_none(), at(9, 0)).sent, 0);
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    // Both posts are back in the queue for after a person looks.
    assert_eq!(w.store.by_status(store::QUEUED).unwrap().len(), 2);
}

#[test]
fn http_429_also_counts_as_spam_risk() {
    let f = fake(|_, _, _| (429, "{}".into()));
    let w = world(json!([tiktok_account("a", 5, 0)]), &[("a-1", "a", "07:00")]);
    w.store.save_token("a", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    assert!(run(&w, &tt, &ig_none(), at(7, 0)).stopped);
}

#[test]
fn transient_error_retries_later_without_stopping() {
    let f = fake(|path, _, n| {
        if n == 1 {
            (500, "oops".into())
        } else if path.contains("status") {
            (200, json!({"data": {"status": "PUBLISH_COMPLETE"}, "error": {"code": "ok"}}).to_string())
        } else {
            (200, json!({"data": {"publish_id": "p"}, "error": {"code": "ok"}}).to_string())
        }
    });
    let w = world(json!([tiktok_account("a", 5, 0)]), &[("a-1", "a", "07:00")]);
    w.store.save_token("a", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    let r = run(&w, &tt, &ig_none(), at(7, 0));
    assert_eq!((r.sent, r.failed, r.stopped), (0, 1, false));
    assert_eq!(run(&w, &tt, &ig_none(), at(7, 10)).sent, 0, "waits 15 minutes");
    assert_eq!(run(&w, &tt, &ig_none(), at(7, 16)).sent, 1);
    assert_eq!(w.store.by_status(store::PUBLISHED).unwrap().len(), 1);
}

#[test]
fn bad_token_pauses_only_that_account() {
    let f = fake(|_, body, _| {
        if body.contains("\"hook 0\"") {
            (401, json!({"error": {"code": "access_token_invalid"}}).to_string())
        } else {
            (200, json!({"data": {"publish_id": "p"}, "error": {"code": "ok"}}).to_string())
        }
    });
    let w = world(
        json!([tiktok_account("a", 5, 0), tiktok_account("b", 5, 0)]),
        &[("a-1", "a", "07:00"), ("b-1", "b", "07:00")],
    );
    w.store.save_token("a", "tok", None, None).unwrap();
    w.store.save_token("b", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    let r = run(&w, &tt, &ig_none(), at(7, 0));
    assert_eq!(r.sent, 1);
    assert!(!r.stopped);
    assert!(w.store.account_state("a", at(7, 0), -7 * 3600).unwrap().paused);
    assert!(!w.store.account_state("b", at(7, 0), -7 * 3600).unwrap().paused);
}

#[test]
fn account_without_token_is_paused_not_posted() {
    let f = ok_tiktok();
    let w = world(json!([tiktok_account("a", 5, 0)]), &[("a-1", "a", "07:00")]);
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    assert_eq!(run(&w, &tt, &ig_none(), at(7, 0)).sent, 0);
    assert!(f.seen.lock().unwrap().is_empty());
    assert!(w.store.account_state("a", at(7, 0), -7 * 3600).unwrap().paused);
}

#[test]
fn crash_mid_send_is_never_retried_blindly() {
    let f = ok_tiktok();
    let w = world(json!([tiktok_account("a", 5, 0)]), &[("a-1", "a", "07:00")]);
    w.store.save_token("a", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    // Simulate a crash after claiming the post but before recording the result.
    w.store.mark_sending(&format!("{DATE}/a-1"), relay::policy::Mode::Inbox, at(7, 0)).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    assert_eq!(run(&w, &tt, &ig_none(), at(7, 5)).sent, 0);
    assert!(f.seen.lock().unwrap().is_empty());
    assert_eq!(w.store.by_status(store::UNKNOWN).unwrap().len(), 1);
}

#[test]
fn ramp_starts_low() {
    let f = ok_tiktok();
    let posts: Vec<(String, &str, String)> = (0..6).map(|i| (format!("a-{i}"), "a", format!("0{i}:00"))).collect();
    let refs: Vec<(&str, &str, &str)> = posts.iter().map(|(p, a, s)| (p.as_str(), *a, s.as_str())).collect();
    let acct = json!([{"handle": "a", "platform": "tiktok", "mode": "direct", "daily_cap": 10, "min_gap_minutes": 0}]);
    let w = world(acct, &refs);
    w.store.save_token("a", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: f.url.clone() };
    let mut sent = 0;
    for h in 0..24 {
        sent += run(&w, &tt, &ig_none(), at(h, 30)).sent;
    }
    assert_eq!(sent, 2, "week 1 of the default ramp is 2 a day");
}

#[test]
fn instagram_carousel_flow() {
    let f = fake(|path, body, n| {
        if path.contains("media_publish") {
            assert!(body.contains("creation_id=c"));
            (200, json!({"id": "live1"}).to_string())
        } else if path.contains("fields=status_code") {
            (200, json!({"status_code": "FINISHED"}).to_string())
        } else if body.contains("media_type=CAROUSEL") {
            assert!(body.contains("children=i1%2Ci2%2Ci3"));
            (200, json!({"id": "c"}).to_string())
        } else {
            assert!(body.contains("is_carousel_item=true"));
            (200, json!({"id": format!("i{n}")}).to_string())
        }
    });
    let acct = json!([{"handle": "ig", "platform": "instagram", "mode": "direct", "daily_cap": 5, "ramp": [5,5,5,5], "ig_user_id": "1784", "min_gap_minutes": 0}]);
    let w = world(acct, &[("ig-1", "ig", "08:00")]);
    w.store.save_token("ig", "tok", None, None).unwrap();
    manifest::import(&w.cfg, &w.store, &w.manifest, DATE).unwrap();
    let tt = TikTok { api_base: "http://127.0.0.1:9".into() };
    let ig = Instagram { api_base: f.url.clone(), poll_secs: 0, max_polls: 3 };
    let r = run(&w, &tt, &ig, at(8, 0));
    assert_eq!(r.sent, 1);
    assert_eq!(w.store.by_status(store::PUBLISHED).unwrap().len(), 1);
}

#[test]
fn import_refuses_unknown_accounts_and_missing_images() {
    let w = world(json!([tiktok_account("a", 5, 0)]), &[("z-1", "z", "07:00")]);
    assert!(manifest::import(&w.cfg, &w.store, &w.manifest, DATE).is_err());
    let w = world(json!([tiktok_account("a", 5, 0)]), &[]);
    let bad = json!([{"post": "a-1", "account": "a", "slot": "07:00", "images": ["/nope.png"]}]).to_string();
    assert!(manifest::import(&w.cfg, &w.store, &bad, DATE).is_err());
    let _ = w.manifest;
}
