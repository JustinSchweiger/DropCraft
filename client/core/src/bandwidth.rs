//! Bandbreitenbegrenzung und Abbruch laufender Übertragungen.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Token-Bucket: höchstens `limit` Bytes pro Sekunde (0 = unbegrenzt).
#[derive(Default)]
pub struct Limiter {
    limit: AtomicU64,
    bucket: Mutex<Option<(Instant, f64)>>,
}

impl Limiter {
    pub fn set_limit(&self, bytes_per_sec: u64) {
        self.limit.store(bytes_per_sec, Ordering::Relaxed);
        *self.bucket.lock().unwrap() = None;
    }

    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Wartet, bis `n` Bytes übertragen werden dürfen.
    pub fn take(&self, n: u64) {
        let mut need = n as f64;
        loop {
            let limit = self.limit() as f64;
            if limit <= 0.0 {
                return;
            }
            let wait = {
                let mut b = self.bucket.lock().unwrap();
                let now = Instant::now();
                let (last, tokens) = b.get_or_insert((now, limit));
                let refill = now.duration_since(*last).as_secs_f64() * limit;
                *tokens = (*tokens + refill).min(limit);
                *last = now;
                if *tokens >= need {
                    *tokens -= need;
                    return;
                }
                need -= *tokens;
                *tokens = 0.0;
                need / limit
            };
            std::thread::sleep(Duration::from_secs_f64(wait.min(0.25)));
        }
    }
}

/// Gemeinsam für alle Konten: Limits für Upload und Download.
#[derive(Default)]
pub struct Bandwidth {
    pub up: Limiter,
    pub down: Limiter,
}

pub type BandwidthRef = Arc<Bandwidth>;

/// Pausieren-Schalter eines Kontos; bricht laufende Übertragungen ab.
pub type PauseFlag = Arc<AtomicBool>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_rate() {
        let l = Limiter::default();
        l.set_limit(200_000); // 200 KB/s, Start mit 1 s Guthaben
        let t = Instant::now();
        for _ in 0..40 {
            l.take(10_000); // 400 KB insgesamt
        }
        let s = t.elapsed().as_secs_f64();
        assert!(s > 0.8 && s < 1.6, "{s}");
        l.set_limit(0);
        let t = Instant::now();
        l.take(1_000_000_000);
        assert!(t.elapsed() < Duration::from_millis(10));
    }
}
