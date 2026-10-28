//! TikTok Content Posting API: photo posts (slideshows) by PULL_FROM_URL.
//!
//! Inbox mode (`MEDIA_UPLOAD`, scope video.upload) sends the slideshow to the
//! account's TikTok inbox; the owner taps the notification, adds a sound and
//! posts. It works before TikTok audits our app. Direct mode (`DIRECT_POST`,
//! scope video.publish) posts publicly once the app passes TikTok's audit;
//! until then TikTok keeps direct posts private.

use serde_json::{json, Value};

use super::{transport_error, Outgoing, Provider, PublishError, Remote};
use crate::policy::{ErrorClass, Mode};

pub struct TikTok {
    pub api_base: String,
}

/// Map a TikTok `error.code` onto what the engine should do.
pub fn classify(code: &str) -> ErrorClass {
    match code {
        "spam_risk_too_many_posts"
        | "spam_risk_too_many_pending_share"
        | "spam_risk_user_banned_from_posting"
        | "spam_risk"
        | "rate_limit_exceeded"
        | "reached_active_user_cap" => ErrorClass::SpamRisk,
        "access_token_invalid" | "scope_not_authorized" | "token_not_authorized_for_specified_scope" => {
            ErrorClass::Auth
        }
        "url_ownership_unverified"
        | "invalid_params"
        | "privacy_level_option_mismatch"
        | "picture_size_check_failed"
        | "file_format_check_failed"
        | "app_version_check_failed"
        | "unaudited_client_can_only_post_to_private_accounts" => ErrorClass::BadPost,
        _ => ErrorClass::Transient,
    }
}

/// Build the init request body.
pub fn init_body(post: &Outgoing) -> Value {
    let mut post_info = json!({
        "title": truncate_utf16(post.title, 90),
        "description": truncate_utf16(post.caption, 4000),
    });
    let post_mode = match post.mode {
        Mode::Direct => {
            post_info["privacy_level"] = json!(post.privacy_level);
            post_info["auto_add_music"] = json!(true);
            "DIRECT_POST"
        }
        Mode::Inbox => "MEDIA_UPLOAD",
    };
    json!({
        "media_type": "PHOTO",
        "post_mode": post_mode,
        "post_info": post_info,
        "source_info": {
            "source": "PULL_FROM_URL",
            "photo_images": post.image_urls,
            "photo_cover_index": 0
        }
    })
}

fn truncate_utf16(s: &str, max: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        out.push(c);
    }
    out
}

fn check_error(v: &Value) -> Result<(), PublishError> {
    let code = v["error"]["code"].as_str().unwrap_or("ok");
    if code == "ok" {
        Ok(())
    } else {
        Err(PublishError {
            class: classify(code),
            message: format!("{code}: {}", v["error"]["message"].as_str().unwrap_or("")),
        })
    }
}

fn post_json(url: &str, token: &str, body: &Value) -> Result<Value, PublishError> {
    let resp = ureq::post(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json; charset=UTF-8")
        .send_json(body.clone());
    let v: Value = match resp {
        Ok(r) => r.into_json().map_err(|e| PublishError { class: ErrorClass::Transient, message: e.to_string() })?,
        Err(ureq::Error::Status(code, r)) => {
            // TikTok returns the real reason in the JSON body even on 4xx.
            let text = r.into_string().unwrap_or_default();
            match serde_json::from_str::<Value>(&text) {
                Ok(v) if v["error"]["code"].is_string() => v,
                _ => return Err(transport_error(ureq::Error::Status(code, ureq::Response::new(code, "", &text).unwrap()))),
            }
        }
        Err(e) => return Err(transport_error(e)),
    };
    check_error(&v)?;
    Ok(v)
}

impl Provider for TikTok {
    fn publish(&self, post: &Outgoing) -> Result<String, PublishError> {
        if post.image_urls.is_empty() || post.image_urls.len() > 35 {
            return Err(PublishError { class: ErrorClass::BadPost, message: "TikTok takes 1 to 35 photos".into() });
        }
        let url = format!("{}/v2/post/publish/content/init/", self.api_base);
        let v = post_json(&url, post.access_token, &init_body(post))?;
        v["data"]["publish_id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| PublishError { class: ErrorClass::Transient, message: format!("no publish_id in {v}") })
    }

    fn status(&self, publish_id: &str, token: &str) -> Result<Remote, PublishError> {
        let url = format!("{}/v2/post/publish/status/fetch/", self.api_base);
        let v = post_json(&url, token, &json!({ "publish_id": publish_id }))?;
        Ok(match v["data"]["status"].as_str().unwrap_or("") {
            "PUBLISH_COMPLETE" => Remote::Published,
            "SEND_TO_USER_INBOX" => Remote::InInbox,
            "FAILED" => Remote::Failed(v["data"]["fail_reason"].as_str().unwrap_or("").to_string()),
            _ => Remote::InProgress,
        })
    }
}

/// OAuth helpers. The client secret comes from TIKTOK_CLIENT_SECRET.
pub fn authorize_url(client_key: &str, redirect_uri: &str, state: &str, scopes: &str) -> String {
    format!(
        "https://www.tiktok.com/v2/auth/authorize/?client_key={}&scope={}&response_type=code&redirect_uri={}&state={}",
        enc(client_key),
        enc(scopes),
        enc(redirect_uri),
        enc(state)
    )
}

pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

pub fn exchange(api_base: &str, form: &[(&str, &str)]) -> anyhow::Result<Tokens> {
    let v: Value = ureq::post(&format!("{api_base}/v2/oauth/token/"))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_form(form)?
        .into_json()?;
    let access = v["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("token exchange failed: {v}"))?;
    Ok(Tokens {
        access_token: access.to_string(),
        refresh_token: v["refresh_token"].as_str().map(String::from),
        expires_in: v["expires_in"].as_i64(),
    })
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outgoing(mode: Mode, urls: &[String]) -> Outgoing<'_> {
        Outgoing {
            image_urls: urls,
            caption: "caption",
            title: "hook",
            mode,
            privacy_level: "PUBLIC_TO_EVERYONE",
            ig_user_id: None,
            access_token: "t",
        }
    }

    #[test]
    fn inbox_body_has_no_privacy() {
        let urls = vec!["https://m.example.com/a.jpg".to_string()];
        let b = init_body(&outgoing(Mode::Inbox, &urls));
        assert_eq!(b["post_mode"], "MEDIA_UPLOAD");
        assert_eq!(b["media_type"], "PHOTO");
        assert_eq!(b["source_info"]["source"], "PULL_FROM_URL");
        assert!(b["post_info"].get("privacy_level").is_none());
    }

    #[test]
    fn direct_body_sets_privacy_and_music() {
        let urls = vec!["https://m.example.com/a.jpg".to_string()];
        let b = init_body(&outgoing(Mode::Direct, &urls));
        assert_eq!(b["post_mode"], "DIRECT_POST");
        assert_eq!(b["post_info"]["privacy_level"], "PUBLIC_TO_EVERYONE");
        assert_eq!(b["post_info"]["auto_add_music"], true);
    }

    #[test]
    fn title_is_cut_to_90_utf16_units() {
        let long = "🙂".repeat(60); // 120 units
        assert_eq!(truncate_utf16(&long, 90).encode_utf16().count(), 90);
    }

    #[test]
    fn spam_codes_stop_the_engine() {
        for c in ["spam_risk_too_many_posts", "spam_risk_too_many_pending_share", "rate_limit_exceeded"] {
            assert_eq!(classify(c), ErrorClass::SpamRisk);
        }
        assert_eq!(classify("access_token_invalid"), ErrorClass::Auth);
        assert_eq!(classify("url_ownership_unverified"), ErrorClass::BadPost);
        assert_eq!(classify("internal_error"), ErrorClass::Transient);
    }
}
