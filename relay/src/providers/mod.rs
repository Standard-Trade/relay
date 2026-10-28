//! Platform clients. Each turns a post (public image URLs + caption) into a
//! platform post, and maps platform errors onto the policy's error classes.

pub mod instagram;
pub mod tiktok;

use crate::policy::{ErrorClass, Mode};

#[derive(Debug)]
pub struct PublishError {
    pub class: ErrorClass,
    pub message: String,
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.class, self.message)
    }
}

pub struct Outgoing<'a> {
    pub image_urls: &'a [String],
    pub caption: &'a str,
    pub title: &'a str,
    pub mode: Mode,
    pub privacy_level: &'a str,
    pub ig_user_id: Option<&'a str>,
    pub access_token: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Remote {
    InProgress,
    /// TikTok inbox: waiting for the account owner to finish it in the app.
    InInbox,
    Published,
    Failed(String),
}

pub trait Provider {
    fn publish(&self, post: &Outgoing) -> Result<String, PublishError>;
    fn status(&self, publish_id: &str, access_token: &str) -> Result<Remote, PublishError>;
}

/// Logs instead of posting. Used for dry runs and tests.
pub struct DryRun;

impl Provider for DryRun {
    fn publish(&self, post: &Outgoing) -> Result<String, PublishError> {
        eprintln!("[dry run] {} images, caption {:?}", post.image_urls.len(), post.caption);
        Ok(format!("dry-{}", post.image_urls.first().map(|s| s.as_str()).unwrap_or("")))
    }
    fn status(&self, _: &str, _: &str) -> Result<Remote, PublishError> {
        Ok(Remote::Published)
    }
}

pub(crate) fn transport_error(e: ureq::Error) -> PublishError {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let class = match code {
                401 | 403 => ErrorClass::Auth,
                429 => ErrorClass::SpamRisk,
                400 | 404 | 422 => ErrorClass::BadPost,
                _ => ErrorClass::Transient,
            };
            // A body can carry a more specific platform code; let callers refine.
            PublishError { class, message: format!("HTTP {code}: {body}") }
        }
        other => PublishError { class: ErrorClass::Transient, message: other.to_string() },
    }
}
