//! Property tests: random multi-day schedules against the policy. Kani proves
//! the rules for any single decision and any 12-step day; these check long
//! runs across many days with day rollover.

use proptest::prelude::*;
use relay::policy::*;

fn simulate(limits: Limits, mode: Mode, offset: i64, start: i64, steps: Vec<i64>) -> Result<(), TestCaseError> {
    let mut now = start;
    let mut s = AccountState { paused: false, started_at: start, day: day_of(now, offset), posted_today: 0, last_post_at: None, pending: 0 };
    let mut per_day: std::collections::HashMap<i64, u32> = Default::default();
    let mut last: Option<i64> = None;
    for step in steps {
        now += step;
        roll_day(&mut s, now, offset);
        // Inbox drafts get finished by a person a few hours later.
        if mode == Mode::Inbox && step > 3 * 3600 {
            s.pending = s.pending.saturating_sub(1);
        }
        if decide(false, &s, &limits, mode, now, start, offset) == Decision::Post {
            let cap = effective_cap(&limits, s.started_at, now, offset);
            record_post(&mut s, mode, now);
            let c = per_day.entry(day_of(now, offset)).or_default();
            *c += 1;
            prop_assert!(*c <= cap);
            prop_assert!(*c <= HARD_MAX);
            prop_assert!(s.pending <= TIKTOK_PENDING_MAX);
            if let Some(l) = last {
                prop_assert!(now - l >= limits.min_gap_secs);
            }
            last = Some(now);
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn long_runs_respect_every_limit(
        cap in 0u32..20,
        ramp in prop::array::uniform4(0u32..20),
        gap in 0i64..7200,
        offset in -14i64..=14,
        start in 1_700_000_000i64..1_900_000_000,
        inbox in any::<bool>(),
        steps in prop::collection::vec(0i64..20_000, 1..400),
    ) {
        let limits = Limits::tiktok(cap, ramp, gap);
        let mode = if inbox { Mode::Inbox } else { Mode::Direct };
        simulate(limits, mode, offset * 3600, start, steps)?;
    }

    #[test]
    fn cap_follows_the_ramp_by_week(ramp in prop::array::uniform4(0u32..12), days in 0i64..60) {
        let limits = Limits::tiktok(100, ramp, 0);
        let start = 1_800_000_000;
        let now = start + days * SECS_PER_DAY;
        let week = (days / 7).min(3) as usize;
        let expected = ramp[week].min(TIKTOK_DAILY_MAX).min(HARD_MAX);
        prop_assert_eq!(effective_cap(&limits, start, now, 0), expected);
    }
}
