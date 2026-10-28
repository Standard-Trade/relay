//! One scheduler pass: post whatever is due and allowed, then check the
//! status of what was sent.

use anyhow::Result;
use std::path::Path;

use crate::config::Config;
use crate::media;
use crate::policy::{self, Decision, ErrorClass};
use crate::providers::{Outgoing, Provider, Remote};
use crate::store::{self, Store};

pub struct Providers<'a> {
    pub tiktok: &'a dyn Provider,
    pub instagram: &'a dyn Provider,
    pub dryrun: &'a dyn Provider,
}

impl<'a> Providers<'a> {
    fn for_platform(&self, platform: &str) -> &'a dyn Provider {
        match platform {
            "tiktok" => self.tiktok,
            "instagram" => self.instagram,
            _ => self.dryrun,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub sent: u32,
    pub waiting: u32,
    pub skipped: u32,
    pub failed: u32,
    pub stopped: bool,
}

pub fn tick(cfg: &Config, store: &Store, providers: &Providers, now: i64) -> Result<TickReport> {
    let offset = cfg.utc_offset();
    let mut report = TickReport::default();
    // Posts left mid-send by a crash are never retried blindly: a person
    // checks the account and requeues or drops them.
    for (p, _, _) in store.by_status(store::SENDING)? {
        store.set_status(&p.id, store::UNKNOWN, "interrupted while sending; check the account")?;
        store.log(now, &p.account, &p.id, "unknown", "interrupted while sending")?;
    }
    for post in store.due(now)? {
        let mut stopped = store.stopped()?;
        if stopped {
            report.stopped = true;
            break;
        }
        let Some(acct) = cfg.account(&post.account) else {
            store.set_status(&post.id, store::DROPPED, "account not in config")?;
            report.skipped += 1;
            continue;
        };
        if cfg.media_base_url.is_empty() && acct.platform != "dryrun" {
            report.skipped += 1;
            continue;
        }
        let mode = acct.mode();
        let limits = acct.limits();
        let state = store.account_state(&acct.handle, now, offset)?;
        match policy::decide(stopped, &state, &limits, mode, now, post.scheduled_at, offset) {
            Decision::Wait(_) => {
                report.waiting += 1;
                continue;
            }
            Decision::Skip(_) => {
                report.skipped += 1;
                continue;
            }
            Decision::Post => {}
        }
        let token = match store.token(&acct.handle)? {
            Some((t, _, _)) => t,
            None if acct.platform == "dryrun" => String::new(),
            None => {
                store.set_paused(&acct.handle, true, "no access token; run `relay login`")?;
                report.skipped += 1;
                continue;
            }
        };
        let mut urls = Vec::with_capacity(post.images.len());
        let mut bad = None;
        for img in &post.images {
            match media::publish_image(Path::new(img), Path::new(&cfg.media_dir)) {
                Ok(name) => urls.push(media::url(&cfg.media_base_url, &name)),
                Err(e) => {
                    bad = Some(e.to_string());
                    break;
                }
            }
        }
        if let Some(e) = bad {
            store.set_status(&post.id, store::DROPPED, &e)?;
            store.log(now, &acct.handle, &post.id, "dropped", &e)?;
            report.failed += 1;
            continue;
        }
        store.ensure_started(&acct.handle, now)?;
        store.mark_sending(&post.id, mode, now)?;
        let out = Outgoing {
            image_urls: &urls,
            caption: &post.caption,
            title: &post.hook,
            mode,
            privacy_level: &acct.privacy_level,
            ig_user_id: acct.ig_user_id.as_deref(),
            access_token: &token,
        };
        match providers.for_platform(&acct.platform).publish(&out) {
            Ok(id) => {
                store.mark_sent(&post.id, mode, now, &id)?;
                store.log(now, &acct.handle, &post.id, "sent", &id)?;
                report.sent += 1;
            }
            Err(e) => {
                let mut st = state;
                stopped = policy::on_error(&mut st, e.class, stopped);
                store.log(now, &acct.handle, &post.id, "error", &e.to_string())?;
                match e.class {
                    ErrorClass::Transient => store.requeue(&post.id, now + 15 * 60)?,
                    ErrorClass::BadPost => store.set_status(&post.id, store::FAILED, &e.message)?,
                    ErrorClass::Auth | ErrorClass::SpamRisk => {
                        store.requeue(&post.id, post.scheduled_at)?;
                        store.set_paused(&acct.handle, true, &e.to_string())?;
                    }
                }
                if stopped {
                    store.set_stopped(true)?;
                    report.stopped = true;
                }
                report.failed += 1;
            }
        }
    }
    check_status(cfg, store, providers, now)?;
    Ok(report)
}

fn check_status(cfg: &Config, store: &Store, providers: &Providers, now: i64) -> Result<()> {
    for (post, publish_id, _) in store.by_status(store::SENT)? {
        let Some(acct) = cfg.account(&post.account) else { continue };
        let token = store.token(&acct.handle)?.map(|t| t.0).unwrap_or_default();
        match providers.for_platform(&acct.platform).status(&publish_id, &token) {
            Ok(Remote::Published) => store.set_status(&post.id, store::PUBLISHED, "")?,
            Ok(Remote::Failed(why)) => {
                store.set_status(&post.id, store::FAILED, &why)?;
                store.log(now, &acct.handle, &post.id, "failed", &why)?;
            }
            Ok(Remote::InProgress | Remote::InInbox) => {}
            Err(e) => store.log(now, &acct.handle, &post.id, "status_error", &e.to_string())?,
        }
    }
    Ok(())
}
