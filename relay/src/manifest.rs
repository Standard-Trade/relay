//! Importing the manifest that `engine/batch.py` writes.

use anyhow::{bail, Result};
use serde::Deserialize;

use crate::config::Config;
use crate::schedule;
use crate::store::{Post, Store, QUEUED};

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub post: String,
    pub account: String,
    pub slot: String,
    pub images: Vec<String>,
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub hook: String,
}

/// Queue every entry for `date`. Returns how many were new.
pub fn import(cfg: &Config, store: &Store, text: &str, date: &str) -> Result<usize> {
    let entries: Vec<Entry> = serde_json::from_str(text)?;
    for e in &entries {
        if cfg.account(&e.account).is_none() {
            bail!("{}: account {} is not in the config", e.post, e.account);
        }
        if e.images.is_empty() {
            bail!("{}: no images", e.post);
        }
        for img in &e.images {
            if !std::path::Path::new(img).is_file() {
                bail!("{}: image {img} does not exist", e.post);
            }
        }
    }
    let mut added = 0;
    for e in entries {
        let id = format!("{date}/{}", e.post);
        let at = schedule::slot_time(date, &e.slot, cfg.utc_offset())? + schedule::jitter_secs(&id, cfg.jitter_minutes);
        let post = Post {
            id,
            account: e.account,
            scheduled_at: at,
            images: e.images,
            caption: e.caption,
            hook: e.hook,
            status: QUEUED.into(),
        };
        if store.enqueue(&post)? {
            added += 1;
        }
    }
    Ok(added)
}
