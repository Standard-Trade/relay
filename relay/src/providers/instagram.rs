//! Instagram Graph API carousels: one container per image, a CAROUSEL
//! container, then media_publish. Works for professional accounts that have a
//! role on our Meta app, before app review.

use serde_json::Value;

use super::{transport_error, Outgoing, Provider, PublishError, Remote};
use crate::policy::ErrorClass;

pub struct Instagram {
    pub api_base: String,
    /// Seconds to wait between container status checks.
    pub poll_secs: u64,
    pub max_polls: u32,
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Value, PublishError> {
    let v: Value = ureq::post(url)
        .send_form(form)
        .map_err(classify_transport)?
        .into_json()
        .map_err(|e| PublishError { class: ErrorClass::Transient, message: e.to_string() })?;
    Ok(v)
}

fn get(url: &str) -> Result<Value, PublishError> {
    ureq::get(url)
        .call()
        .map_err(classify_transport)?
        .into_json()
        .map_err(|e| PublishError { class: ErrorClass::Transient, message: e.to_string() })
}

/// Meta error codes: 4/17/32/613 are rate limits, 190 is a bad token,
/// 9 / 368 are "action blocked" (spam), 2207xxx are media problems.
pub fn classify_meta(code: i64, subcode: i64) -> ErrorClass {
    match code {
        4 | 17 | 32 | 613 | 9 | 368 => ErrorClass::SpamRisk,
        190 | 10 | 200 => ErrorClass::Auth,
        100 | 36003 | 352 | 9004 => ErrorClass::BadPost,
        _ if (2_207_000..2_208_000).contains(&subcode) => ErrorClass::BadPost,
        _ => ErrorClass::Transient,
    }
}

fn classify_transport(e: ureq::Error) -> PublishError {
    if let ureq::Error::Status(code, resp) = e {
        let text = resp.into_string().unwrap_or_default();
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let c = v["error"]["code"].as_i64().unwrap_or(0);
            let s = v["error"]["error_subcode"].as_i64().unwrap_or(0);
            return PublishError { class: classify_meta(c, s), message: format!("HTTP {code}: {text}") };
        }
        return transport_error(ureq::Error::Status(code, ureq::Response::new(code, "", &text).unwrap()));
    }
    transport_error(e)
}

fn id_of(v: &Value) -> Result<String, PublishError> {
    v["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| PublishError { class: ErrorClass::Transient, message: format!("no id in {v}") })
}

impl Instagram {
    fn wait_ready(&self, container: &str, token: &str) -> Result<(), PublishError> {
        for _ in 0..self.max_polls {
            let v = get(&format!("{}/{container}?fields=status_code&access_token={token}", self.api_base))?;
            match v["status_code"].as_str().unwrap_or("") {
                "FINISHED" | "PUBLISHED" => return Ok(()),
                "ERROR" | "EXPIRED" => {
                    return Err(PublishError { class: ErrorClass::BadPost, message: format!("container {container}: {v}") })
                }
                _ => std::thread::sleep(std::time::Duration::from_secs(self.poll_secs)),
            }
        }
        Err(PublishError { class: ErrorClass::Transient, message: format!("container {container} not ready") })
    }
}

impl Provider for Instagram {
    fn publish(&self, post: &Outgoing) -> Result<String, PublishError> {
        let user = post
            .ig_user_id
            .ok_or_else(|| PublishError { class: ErrorClass::BadPost, message: "no ig_user_id".into() })?;
        let n = post.image_urls.len();
        if !(2..=20).contains(&n) {
            return Err(PublishError { class: ErrorClass::BadPost, message: "carousels take 2 to 20 images".into() });
        }
        let token = post.access_token;
        let media = format!("{}/{user}/media", self.api_base);
        let mut children = Vec::with_capacity(n);
        for url in post.image_urls {
            let v = post_form(&media, &[("image_url", url), ("is_carousel_item", "true"), ("access_token", token)])?;
            children.push(id_of(&v)?);
        }
        for c in &children {
            self.wait_ready(c, token)?;
        }
        let kids = children.join(",");
        let v = post_form(
            &media,
            &[("media_type", "CAROUSEL"), ("children", &kids), ("caption", post.caption), ("access_token", token)],
        )?;
        let carousel = id_of(&v)?;
        self.wait_ready(&carousel, token)?;
        let v = post_form(
            &format!("{}/{user}/media_publish", self.api_base),
            &[("creation_id", &carousel), ("access_token", token)],
        )?;
        id_of(&v)
    }

    fn status(&self, _publish_id: &str, _token: &str) -> Result<Remote, PublishError> {
        // media_publish only returns once the post is live.
        Ok(Remote::Published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_codes() {
        assert_eq!(classify_meta(4, 0), ErrorClass::SpamRisk);
        assert_eq!(classify_meta(368, 0), ErrorClass::SpamRisk);
        assert_eq!(classify_meta(190, 0), ErrorClass::Auth);
        assert_eq!(classify_meta(0, 2_207_026), ErrorClass::BadPost);
        assert_eq!(classify_meta(1, 0), ErrorClass::Transient);
    }
}
