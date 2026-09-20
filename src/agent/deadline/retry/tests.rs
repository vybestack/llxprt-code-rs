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
            let long_provider_minimum = MAX_SLEEP + Duration::from_secs(7);
            assert_eq!(
                delay(attempt, Some(long_provider_minimum), seed),
                long_provider_minimum,
                "Retry-After remains a minimum above the local backoff cap",
            );
            let provider_minimum = window + Duration::from_secs(7);
            assert_eq!(
                delay(attempt, Some(provider_minimum), seed),
                provider_minimum,
                "Retry-After is a minimum, not a jitter ceiling",
            );
        }
    }
}
