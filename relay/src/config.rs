//! relay.json: accounts, platforms, media hosting. Secrets come from the
//! environment, never from this file.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::policy::{Limits, Mode, TIKTOK_DAILY_MAX, TIKTOK_PENDING_MAX};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    /// Hours from UTC for "local day" and time slots, e.g. -7 for PDT.
    pub utc_offset_hours: i64,
    /// Folder the media server serves; relay copies post images here.
    pub media_dir: String,
    /// Public base URL that serves `media_dir` (a domain verified in the
    /// TikTok developer portal for PULL_FROM_URL).
    pub media_base_url: String,
    /// Random-looking spread added to each slot, in minutes (deterministic per post).
    #[serde(default = "default_jitter")]
    pub jitter_minutes: i64,
    pub accounts: Vec<AccountConfig>,
    #[serde(default)]
    pub tiktok: Option<TikTokConfig>,
    #[serde(default)]
    pub instagram: Option<InstagramConfig>,
}

fn default_jitter() -> i64 {
    7
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AccountConfig {
    pub handle: String,
    /// "tiktok", "instagram" or "dryrun".
    pub platform: String,
    /// "inbox" (TikTok draft the account owner finishes) or "direct".
    #[serde(default = "default_mode")]
    pub mode: String,
    pub daily_cap: u32,
    #[serde(default = "default_ramp")]
    pub ramp: [u32; 4],
    #[serde(default = "default_gap")]
    pub min_gap_minutes: i64,
    /// TikTok privacy level for direct posts.
    #[serde(default = "default_privacy")]
    pub privacy_level: String,
    /// Instagram professional account id.
    #[serde(default)]
    pub ig_user_id: Option<String>,
    /// Daily posting times, "HH:MM" local. batch.py assigns posts to them.
    #[serde(default)]
    pub slots: Vec<String>,
    /// Slideshow style for this account (a file in styles/).
    #[serde(default)]
    pub style: String,
    /// What this account's pages are about, for script writing.
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

#[derive(Debug, Deserialize, Serialize)]
pub struct TikTokConfig {
    pub client_key: String,
    pub redirect_uri: String,
    #[serde(default = "default_tiktok_base")]
    pub api_base: String,
}

fn default_tiktok_base() -> String {
    "https://open.tiktokapis.com".into()
}

#[derive(Debug, Deserialize, Serialize)]
pub struct InstagramConfig {
    #[serde(default = "default_ig_base")]
    pub api_base: String,
}

fn default_ig_base() -> String {
    "https://graph.instagram.com/v23.0".into()
}

impl Config {
    /// A blank config for first-time setup.
    pub fn blank(media_dir: &str) -> Config {
        Config {
            utc_offset_hours: -7,
            media_dir: media_dir.into(),
            media_base_url: String::new(),
            jitter_minutes: default_jitter(),
            accounts: vec![],
            tiktok: None,
            instagram: None,
        }
    }

    /// Load the config, or a blank one if the file doesn't exist yet.
    pub fn load_or_blank(path: &str) -> Result<Config> {
        if std::path::Path::new(path).exists() {
            Config::load(path)
        } else {
            let dir = std::path::Path::new(path).parent().map(|p| p.join("media")).unwrap_or_else(|| "media".into());
            Ok(Config::blank(&dir.to_string_lossy()))
        }
    }

    pub fn load(path: &str) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let cfg: Config = serde_json::from_str(&text).with_context(|| format!("parsing {path}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for a in &self.accounts {
            if !seen.insert(&a.handle) {
                bail!("account {} is listed twice", a.handle);
            }
            if !["tiktok", "instagram", "dryrun"].contains(&a.platform.as_str()) {
                bail!("{}: platform must be tiktok, instagram or dryrun", a.handle);
            }
            if !["inbox", "direct"].contains(&a.mode.as_str()) {
                bail!("{}: mode must be inbox or direct", a.handle);
            }
            if a.platform == "instagram" && a.ig_user_id.is_none() {
                bail!("{}: instagram accounts need ig_user_id", a.handle);
            }
            for s in &a.slots {
                crate::schedule::parse_slot(s).with_context(|| format!("{}: slot {s}", a.handle))?;
            }
            if a.handle.is_empty() || !a.handle.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
                bail!("handle {:?} may only use letters, digits, _ and .", a.handle);
            }
            if a.min_gap_minutes < 0 {
                bail!("{}: min_gap_minutes can't be negative", a.handle);
            }
        }
        if !self.media_base_url.is_empty() && !self.media_base_url.starts_with("https://") {
            bail!("media_base_url must be https (platforms fetch images from it)");
        }
        Ok(())
    }

    /// Write the config atomically (temp file + rename).
    pub fn save(&self, path: &str) -> Result<()> {
        self.validate()?;
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)? + "\n")?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn utc_offset(&self) -> i64 {
        self.utc_offset_hours * 3600
    }

    pub fn account(&self, handle: &str) -> Option<&AccountConfig> {
        self.accounts.iter().find(|a| a.handle == handle)
    }
}

impl AccountConfig {
    pub fn mode(&self) -> Mode {
        if self.platform == "tiktok" && self.mode == "inbox" {
            Mode::Inbox
        } else {
            Mode::Direct
        }
    }

    pub fn limits(&self) -> Limits {
        let platform_max = match self.platform.as_str() {
            "tiktok" => TIKTOK_DAILY_MAX,
            // Instagram allows far more API posts a day; our hard max applies.
            _ => crate::policy::HARD_MAX,
        };
        Limits {
            daily_cap: self.daily_cap,
            ramp: self.ramp,
            platform_max,
            min_gap_secs: self.min_gap_minutes * 60,
            pending_max: TIKTOK_PENDING_MAX,
        }
    }
}
