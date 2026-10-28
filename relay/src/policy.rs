//! The posting policy: decides whether an account may post right now.
//!
//! This module is pure (no IO, no clock, no allocation) so it can be proved
//! with Kani. Every safety rule Andy cares about lives here:
//!
//! 1. A stopped engine posts nothing.
//! 2. A paused account posts nothing.
//! 3. No account posts more than its cap in one local day. The cap is the
//!    smallest of: the account's own cap, the ramp for its age, and the
//!    platform's hard maximum.
//! 4. Two posts from one account are at least `min_gap_secs` apart.
//! 5. In inbox mode, an account never has more than `pending_max` drafts
//!    waiting to be finished in the app.
//! 6. Nothing posts before its scheduled time.

pub const SECS_PER_DAY: i64 = 86_400;

/// TikTok's per-creator daily ceiling (`spam_risk_too_many_posts`) is about 15.
pub const TIKTOK_DAILY_MAX: u32 = 15;
/// TikTok allows at most 5 pending inbox shares in 24 hours.
pub const TIKTOK_PENDING_MAX: u32 = 5;
/// Our own hard ceiling, below every platform limit.
pub const HARD_MAX: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Post publicly straight away (needs an audited TikTok app, or Instagram).
    Direct,
    /// Send to the account's TikTok inbox; a person taps to finish the post.
    Inbox,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The account's own cap per local day.
    pub daily_cap: u32,
    /// Ramp caps by account week: week 0, 1, 2, then `ramp[3]` from week 3 on.
    pub ramp: [u32; 4],
    /// Platform ceiling per day.
    pub platform_max: u32,
    /// Minimum seconds between two posts from one account.
    pub min_gap_secs: i64,
    /// Inbox mode: max drafts waiting at once.
    pub pending_max: u32,
}

impl Limits {
    pub fn tiktok(daily_cap: u32, ramp: [u32; 4], min_gap_secs: i64) -> Self {
        Limits { daily_cap, ramp, platform_max: TIKTOK_DAILY_MAX, min_gap_secs, pending_max: TIKTOK_PENDING_MAX }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountState {
    pub paused: bool,
    /// Unix seconds when the account started posting (sets its ramp week).
    pub started_at: i64,
    /// Local day number (see `day_of`) that `posted_today` counts.
    pub day: i64,
    pub posted_today: u32,
    pub last_post_at: Option<i64>,
    /// Inbox drafts sent but not yet finished by a person.
    pub pending: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Stopped,
    Paused,
    CapReached,
    PendingFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Post,
    /// Try again at this time.
    Wait(i64),
    Skip(Skip),
}

/// Local day number for a unix time and a UTC offset in seconds.
pub fn day_of(now: i64, utc_offset: i64) -> i64 {
    (now + utc_offset).div_euclid(SECS_PER_DAY)
}

/// Start of the next local day, as unix seconds.
pub fn next_day_start(now: i64, utc_offset: i64) -> i64 {
    (day_of(now, utc_offset) + 1) * SECS_PER_DAY - utc_offset
}

/// Today's cap for an account: the smallest of its own cap, its ramp step,
/// the platform maximum and our hard maximum. The ramp week is counted in
/// whole local days, so the cap never changes in the middle of a day.
pub fn effective_cap(limits: &Limits, started_at: i64, now: i64, utc_offset: i64) -> u32 {
    let days = (day_of(now, utc_offset) - day_of(started_at, utc_offset)).max(0);
    let week = (days / 7).min(3) as usize;
    limits.daily_cap.min(limits.ramp[week]).min(limits.platform_max).min(HARD_MAX)
}

/// Reset the day counter when the local day has changed.
pub fn roll_day(state: &mut AccountState, now: i64, utc_offset: i64) {
    let today = day_of(now, utc_offset);
    if today != state.day {
        state.day = today;
        state.posted_today = 0;
    }
}

/// Decide what an account may do at `now`. `state` must already be rolled to
/// today with `roll_day`.
pub fn decide(
    stopped: bool,
    state: &AccountState,
    limits: &Limits,
    mode: Mode,
    now: i64,
    scheduled_at: i64,
    utc_offset: i64,
) -> Decision {
    if stopped {
        return Decision::Skip(Skip::Stopped);
    }
    if state.paused {
        return Decision::Skip(Skip::Paused);
    }
    if state.day != day_of(now, utc_offset) {
        // Not rolled: refuse rather than trust a stale counter.
        return Decision::Wait(now);
    }
    if state.posted_today >= effective_cap(limits, state.started_at, now, utc_offset) {
        return Decision::Wait(next_day_start(now, utc_offset));
    }
    if mode == Mode::Inbox && state.pending >= limits.pending_max.min(TIKTOK_PENDING_MAX) {
        return Decision::Skip(Skip::PendingFull);
    }
    if now < scheduled_at {
        return Decision::Wait(scheduled_at);
    }
    if let Some(last) = state.last_post_at {
        let ready = last.saturating_add(limits.min_gap_secs.max(0));
        if now < ready {
            return Decision::Wait(ready);
        }
    }
    Decision::Post
}

/// Record a successful post.
pub fn record_post(state: &mut AccountState, mode: Mode, now: i64) {
    state.posted_today = state.posted_today.saturating_add(1);
    state.last_post_at = Some(now);
    if mode == Mode::Inbox {
        state.pending = state.pending.saturating_add(1);
    }
}

/// What a platform error means for the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// Retry later; the account is fine.
    Transient,
    /// The platform flagged volume or spam: pause the account and stop
    /// everything until a person looks.
    SpamRisk,
    /// Token expired or revoked: pause the account.
    Auth,
    /// This post is bad (bad image, bad caption): drop the post.
    BadPost,
}

/// Apply an error to the engine. Returns the new `stopped` flag.
pub fn on_error(state: &mut AccountState, class: ErrorClass, stopped: bool) -> bool {
    match class {
        ErrorClass::SpamRisk => {
            state.paused = true;
            true
        }
        ErrorClass::Auth => {
            state.paused = true;
            stopped
        }
        ErrorClass::Transient | ErrorClass::BadPost => stopped,
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn any_limits() -> Limits {
        let l = Limits {
            daily_cap: kani::any(),
            ramp: kani::any(),
            platform_max: kani::any(),
            min_gap_secs: kani::any(),
            pending_max: kani::any(),
        };
        kani::assume(l.min_gap_secs >= 0 && l.min_gap_secs <= SECS_PER_DAY);
        l
    }

    fn any_time() -> i64 {
        let t: i64 = kani::any();
        // 1970 to about 2100, the range real clocks produce.
        kani::assume(t >= 0 && t < 4_102_444_800);
        t
    }

    fn any_offset() -> i64 {
        let o: i64 = kani::any();
        kani::assume(o >= -14 * 3600 && o <= 14 * 3600);
        o
    }

    fn any_state(now: i64, offset: i64) -> AccountState {
        let s = AccountState {
            paused: kani::any(),
            started_at: any_time(),
            day: day_of(now, offset),
            posted_today: kani::any(),
            last_post_at: if kani::any() { Some(any_time()) } else { None },
            pending: kani::any(),
        };
        s
    }

    fn any_mode() -> Mode {
        if kani::any() { Mode::Direct } else { Mode::Inbox }
    }

    /// Rules 1-6 for a single decision: if the policy says Post, every rule
    /// holds, and after recording the post the day count is within the cap.
    #[kani::proof]
    fn post_obeys_every_rule() {
        let now = any_time();
        let offset = any_offset();
        let limits = any_limits();
        let mut s = any_state(now, offset);
        let stopped: bool = kani::any();
        let mode = any_mode();
        let scheduled = any_time();

        if decide(stopped, &s, &limits, mode, now, scheduled, offset) == Decision::Post {
            assert!(!stopped);
            assert!(!s.paused);
            assert!(now >= scheduled);
            if let Some(last) = s.last_post_at {
                assert!(now - last >= limits.min_gap_secs);
            }
            if mode == Mode::Inbox {
                assert!(s.pending < TIKTOK_PENDING_MAX);
            }
            let cap = effective_cap(&limits, s.started_at, now, offset);
            record_post(&mut s, mode, now);
            assert!(s.posted_today <= cap);
            assert!(s.posted_today <= HARD_MAX);
            assert!(s.posted_today <= TIKTOK_DAILY_MAX);
        }
    }

    /// The cap never exceeds any of its inputs or our hard maximum.
    #[kani::proof]
    fn cap_is_minimum_of_limits() {
        let limits = any_limits();
        let now = any_time();
        let started = any_time();
        let cap = effective_cap(&limits, started, now, any_offset());
        assert!(cap <= limits.daily_cap);
        assert!(cap <= limits.platform_max);
        assert!(cap <= HARD_MAX);
        assert!(limits.ramp.iter().any(|&r| cap <= r));
    }

    /// Inductive proof that the day cap holds forever. Invariant: the
    /// account's counter is for today and is within today's cap. If it holds
    /// at `now`, it holds after any later step (same day or a new day). With
    /// the base case (a new account has 0 posts today) this covers every day
    /// of an unbounded run.
    #[kani::proof]
    fn cap_invariant_is_inductive() {
        let offset = any_offset();
        let limits = any_limits();
        let now = any_time();
        let mut s = any_state(now, offset);
        kani::assume(s.posted_today <= effective_cap(&limits, s.started_at, now, offset));
        let later: i64 = kani::any();
        kani::assume(later >= now && later < 4_102_444_800);
        roll_day(&mut s, later, offset);
        let mode = any_mode();
        if decide(kani::any(), &s, &limits, mode, later, kani::any(), offset) == Decision::Post {
            record_post(&mut s, mode, later);
        }
        assert!(s.day == day_of(later, offset));
        assert!(s.posted_today <= effective_cap(&limits, s.started_at, later, offset));
    }

    /// The cap depends only on the local day, so it can't change mid-day.
    #[kani::proof]
    fn cap_is_constant_within_a_day() {
        let offset = any_offset();
        let limits = any_limits();
        let started = any_time();
        let a = any_time();
        let b = any_time();
        kani::assume(day_of(a, offset) == day_of(b, offset));
        assert!(effective_cap(&limits, started, a, offset) == effective_cap(&limits, started, b, offset));
    }

    /// The same property as a concrete run: 4 posting attempts in one day.
    #[kani::proof]
    #[kani::unwind(5)]
    fn a_day_never_exceeds_cap() {
        let offset = any_offset();
        let limits = any_limits();
        let mut now = any_time();
        let day = day_of(now, offset);
        let mut s = AccountState {
            paused: false,
            started_at: any_time(),
            day,
            posted_today: 0,
            last_post_at: None,
            pending: 0,
        };
        let mode = any_mode();
        let mut posts: u32 = 0;
        for _ in 0..4 {
            let step: i64 = kani::any();
            kani::assume(step >= 0 && step <= SECS_PER_DAY);
            now += step;
            kani::assume(day_of(now, offset) == day);
            roll_day(&mut s, now, offset);
            if decide(false, &s, &limits, mode, now, 0, offset) == Decision::Post {
                record_post(&mut s, mode, now);
                posts += 1;
            }
        }
        // The cap is fixed for the whole local day, so it bounds every post.
        assert!(posts <= effective_cap(&limits, s.started_at, now, offset));
        assert!(posts <= HARD_MAX);
    }

    /// A spam-risk error stops the whole engine and pauses the account, and
    /// nothing posts afterwards.
    #[kani::proof]
    fn spam_risk_stops_everything() {
        let now = any_time();
        let offset = any_offset();
        let limits = any_limits();
        let mut s = any_state(now, offset);
        let stopped = on_error(&mut s, ErrorClass::SpamRisk, kani::any());
        assert!(stopped);
        assert!(s.paused);
        let d = decide(stopped, &s, &limits, any_mode(), now, any_time(), offset);
        assert!(d != Decision::Post);
    }

    /// Rolling the day only resets the counter on a new day.
    #[kani::proof]
    fn roll_day_resets_only_on_new_day() {
        let now = any_time();
        let offset = any_offset();
        let mut s = any_state(now, offset);
        let before = s;
        s.day = kani::any();
        let old_day = s.day;
        roll_day(&mut s, now, offset);
        assert!(s.day == day_of(now, offset));
        if old_day == day_of(now, offset) {
            assert!(s.posted_today == before.posted_today);
        } else {
            assert!(s.posted_today == 0);
        }
    }
}
