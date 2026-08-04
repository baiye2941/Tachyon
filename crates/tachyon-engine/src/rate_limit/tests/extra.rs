use super::*;

#[test]
fn cancel_guard_rolls_back_and_disarm_preserves_debt() {
    let debt = AtomicU64::new(10);
    {
        let _guard = CancelGuard {
            debt: &debt,
            bytes: Some(4),
        };
    }
    assert_eq!(debt.load(Ordering::Acquire), 6);

    let guard = CancelGuard {
        debt: &debt,
        bytes: Some(3),
    };
    guard.disarm();
    assert_eq!(debt.load(Ordering::Acquire), 6);
}

#[tokio::test]
async fn update_rate_from_unlimited_resets_debt() {
    let limiter = RateLimiter::new(0);
    limiter.update_rate(100);
    assert_eq!(limiter.bytes_per_sec(), 100);
    limiter.acquire(50).await;
    assert_eq!(limiter.bytes_per_sec(), 100);
}
