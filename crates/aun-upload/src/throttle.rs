//! Shared bandwidth cap (token bucket) across all parallel uploads, adjustable at runtime
//! (e.g. a lower cap during the day, unlimited overnight — 19.2 schedules).

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

#[derive(Debug)]
struct Bucket {
    rate: Option<u64>,
    tokens: f64,
    last: Instant,
}

/// Bytes-per-second limiter. `None` means unlimited.
#[derive(Debug)]
pub struct Throttle {
    inner: Mutex<Bucket>,
}

impl Throttle {
    pub fn new(bytes_per_sec: Option<u64>) -> Self {
        Self {
            inner: Mutex::new(Bucket {
                rate: bytes_per_sec.filter(|r| *r > 0),
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    pub fn set_rate(&self, bytes_per_sec: Option<u64>) {
        if let Ok(mut b) = self.inner.lock() {
            b.rate = bytes_per_sec.filter(|r| *r > 0);
            b.tokens = b
                .tokens
                .min(b.rate.unwrap_or(0) as f64)
                .max(-(b.rate.unwrap_or(0) as f64));
        }
    }

    pub fn rate(&self) -> Option<u64> {
        self.inner.lock().ok().and_then(|b| b.rate)
    }

    /// Waits until `n` bytes may be sent. Debt model: a send may overdraw the bucket and later
    /// callers wait it off, so any piece size works; the bucket holds at most one second of burst.
    pub async fn acquire(&self, n: u64) {
        loop {
            let wait = {
                let Ok(mut b) = self.inner.lock() else { return };
                let Some(rate) = b.rate else { return };
                let rate = rate as f64;
                let now = Instant::now();
                let elapsed = now.duration_since(b.last).as_secs_f64();
                b.last = now;
                b.tokens = (b.tokens + elapsed * rate).min(rate);
                if b.tokens >= 0.0 {
                    b.tokens -= n as f64;
                    return;
                }
                Duration::from_secs_f64((-b.tokens / rate).max(0.001))
            };
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn caps_average_rate() {
        let t = Throttle::new(Some(100_000));
        let start = Instant::now();
        for _ in 0..10 {
            t.acquire(64 * 1024).await;
        }
        // 640 KiB at 100 kB/s ≈ 6.5 s
        let secs = start.elapsed().as_secs_f64();
        assert!((5.5..7.5).contains(&secs), "{secs}");
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_never_waits_and_rate_can_change() {
        let t = Throttle::new(None);
        let start = Instant::now();
        t.acquire(10_000_000).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        t.set_rate(Some(1000));
        assert_eq!(t.rate(), Some(1000));
        t.acquire(500).await; // overdraws
        t.acquire(1).await; // waits the debt off
        assert!(start.elapsed() >= Duration::from_millis(400));
    }
}
