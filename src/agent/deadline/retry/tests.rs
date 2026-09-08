use super::*;

#[test]
fn exponential_equal_jitter_bounds_and_hint_minimum() {
    for attempt in 1..MAX_ATTEMPTS {
        let window = Duration::from_millis(1_000 << (attempt - 1));
        for seed in [0, 1, 500, 1_000, 2_000, u64::MAX] {
            let wait = delay(attempt, None, seed);
            let minimum = window / 2;
            assert!(wait >= minimum);
            assert!(wait <= window);
            assert_eq!(delay(attempt, Some(MAX_SLEEP), seed), MAX_SLEEP);
        }
    }
}
