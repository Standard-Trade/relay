//! The setup page's API over real HTTP.

use relay::web::{handle_public, handle_setup, App, Paths};
use serde_json::{json, Value};
use std::sync::Arc;

struct Servers {
    setup: String,
    public: String,
    dir: tempfile::TempDir,
}

fn start() -> Servers {
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n).to_string_lossy().to_string();
    let app = Arc::new(App::new(Paths { config: p("relay.json"), db: p("relay.db"), secrets: p("relay.secrets.json") }));
    let setup = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let public = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let s = Servers {
        setup: format!("http://{}", setup.server_addr().to_ip().unwrap()),
        public: format!("http://{}", public.server_addr().to_ip().unwrap()),
        dir,
    };
    let a = app.clone();
    std::thread::spawn(move || {
        for r in setup.incoming_requests() {
            handle_setup(&a, r);
        }
    });
    let media = s.dir.path().join("media");
    std::thread::spawn(move || {
        for r in public.incoming_requests() {
            handle_public(&app, &media, r);
        }
    });
    s
}

fn post(base: &str, path: &str, body: Value) -> Result<Value, (u16, String)> {
    match ureq::post(&format!("{base}{path}")).set("X-Relay", "1").send_json(body) {
        Ok(r) => Ok(r.into_json().unwrap()),
        Err(ureq::Error::Status(c, r)) => Err((c, r.into_string().unwrap())),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn onboarding_flow() {
    let s = start();
    // Blank state before any config exists.
    let st: Value = ureq::get(&format!("{}/api/state", s.setup)).call().unwrap().into_json().unwrap();
    assert_eq!(st["accounts"].as_array().unwrap().len(), 0);
    let page = ureq::get(&format!("{}/", s.setup)).call().unwrap().into_string().unwrap();
    assert!(page.contains("<title>Relay setup</title>"));

    // Step 1.
    let st = post(&s.setup, "/api/settings", json!({
        "utc_offset_hours": -7, "media_base_url": "https://media.example.com/",
        "jitter_minutes": 7, "tiktok_client_key": "ck", "tiktok_client_secret": "shh"
    })).unwrap();
    assert_eq!(st["settings"]["redirect_uri"], "https://media.example.com/callback");
    assert_eq!(st["settings"]["tiktok_secret_set"], true);
    let cfg = std::fs::read_to_string(s.dir.path().join("relay.json")).unwrap();
    assert!(!cfg.contains("shh"), "the secret never goes in relay.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(s.dir.path().join("relay.secrets.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Step 2: add, edit, validate.
    let acct = json!({"handle": "golftips4", "platform": "tiktok", "mode": "inbox", "daily_cap": 8,
        "ramp": [2,4,6,8], "min_gap_minutes": 90, "slots": ["07:00", "09:00"], "style": "cartoon-cover", "angle": "tips"});
    let st = post(&s.setup, "/api/account", acct.clone()).unwrap();
    let a = &st["accounts"][0];
    assert_eq!(a["config"]["handle"], "golftips4");
    assert_eq!(a["connected"], false);
    assert_eq!(a["cap_today"], 2, "a new account starts at week 1 of the ramp");
    let mut bad = acct.clone();
    bad["slots"] = json!(["25:00"]);
    assert!(post(&s.setup, "/api/account", bad).is_err());
    let mut bad = acct.clone();
    bad["handle"] = json!("x/../y");
    assert!(post(&s.setup, "/api/account", bad).is_err());

    // Connect: the sign-in link carries a state that the callback must match.
    let url: Value = ureq::get(&format!("{}/api/login-url?handle=golftips4", s.setup)).call().unwrap().into_json().unwrap();
    let url = url["url"].as_str().unwrap();
    assert!(url.starts_with("https://www.tiktok.com/v2/auth/authorize/"));
    assert!(url.contains("redirect_uri=https%3A%2F%2Fmedia.example.com%2Fcallback"));
    let forged = ureq::get(&format!("{}/callback?code=x&state=golftips4.forged", s.public)).call();
    assert!(matches!(forged, Err(ureq::Error::Status(400, _))));

    // Pause, stop, delete.
    let st = post(&s.setup, "/api/action", json!({"do": "pause", "handle": "golftips4"})).unwrap();
    assert_eq!(st["accounts"][0]["paused"], true);
    let st = post(&s.setup, "/api/action", json!({"do": "stop"})).unwrap();
    assert_eq!(st["stopped"], true);
    let st = post(&s.setup, "/api/account/delete", json!({"handle": "golftips4"})).unwrap();
    assert_eq!(st["accounts"].as_array().unwrap().len(), 0);
}

#[test]
fn writes_need_the_header_and_setup_is_local_only() {
    let s = start();
    let r = ureq::post(&format!("{}/api/action", s.setup)).send_json(json!({"do": "stop"}));
    assert!(matches!(r, Err(ureq::Error::Status(403, _))));
    // A request addressed to another host name (as through a tunnel) is refused.
    let r = ureq::get(&format!("{}/api/state", s.setup)).set("Host", "media.example.com").call();
    assert!(matches!(r, Err(ureq::Error::Status(403, _))));
    // The public side serves no setup API.
    let r = ureq::get(&format!("{}/api/state", s.public)).call();
    assert!(matches!(r, Err(ureq::Error::Status(404, _))));
}

#[test]
fn remote_setup_needs_a_login_and_accepts_pushed_posts() {
    std::env::set_var("RELAY_ADMIN_HOST", "admin.example.com");
    let s = start();
    std::env::remove_var("RELAY_ADMIN_HOST");
    let secrets = s.dir.path().join("relay.secrets.json").to_string_lossy().to_string();
    relay::web::Secrets::set_password(&secrets, "a long test password").unwrap();
    // Seed one account locally.
    post(&s.setup, "/api/settings", json!({"media_base_url": "https://media.example.com"})).unwrap();
    post(&s.setup, "/api/account", json!({"handle": "a", "platform": "dryrun", "daily_cap": 5, "slots": ["07:00"]})).unwrap();

    let remote = |path: &str| ureq::get(&format!("{}{path}", s.setup)).set("Host", "admin.example.com");
    let page = remote("/").call().unwrap().into_string().unwrap();
    assert!(page.contains("Relay sign-in"), "unauthenticated remote visitors get the login page");
    assert!(matches!(remote("/api/state").call(), Err(ureq::Error::Status(401, _))));
    // Other host names are still refused outright.
    let r = ureq::get(&format!("{}/api/state", s.setup)).set("Host", "evil.example.com").call();
    assert!(matches!(r, Err(ureq::Error::Status(403, _))));

    let login = |pw: &str| ureq::post(&format!("{}/api/login", s.setup)).set("Host", "admin.example.com").set("X-Relay", "1").send_json(json!({"password": pw}));
    assert!(matches!(login("wrong password!!"), Err(ureq::Error::Status(401, _))));
    let ok = login("a long test password").unwrap();
    let cookie = ok.header("Set-Cookie").unwrap().to_string();
    assert!(cookie.contains("HttpOnly") && cookie.contains("Secure") && cookie.contains("SameSite=Strict"));
    let token = cookie.split(';').next().unwrap().trim_start_matches("relay_session=").to_string();

    // A 1x1 PNG.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let r: Value = ureq::post(&format!("{}/api/import", s.setup))
        .set("Host", "admin.example.com").set("X-Relay", "1").set("X-Relay-Session", &token)
        .send_json(json!({"date": "2026-10-26", "posts": [{"post": "a-1", "account": "a", "slot": "07:00", "caption": "c", "hook": "h", "images": [png, png]}]}))
        .unwrap().into_json().unwrap();
    assert_eq!(r["queued"], 1);
    let st: Value = remote("/api/state").set("Cookie", &format!("relay_session={token}")).call().unwrap().into_json().unwrap();
    assert_eq!(st["queue"].as_array().unwrap().len(), 1);
    // Path tricks in post names are refused.
    let bad = ureq::post(&format!("{}/api/import", s.setup))
        .set("Host", "admin.example.com").set("X-Relay", "1").set("X-Relay-Session", &token)
        .send_json(json!({"date": "2026-10-26", "posts": [{"post": "../x", "account": "a", "slot": "07:00", "images": [png]}]}));
    assert!(bad.is_err());
}
