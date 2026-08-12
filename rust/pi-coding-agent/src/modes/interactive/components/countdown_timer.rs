//! Countdown timer for dialog components, port of
//! `components/countdown-timer.ts`.
//!
//! ponytail: the JS version drives itself with setInterval. Here the host
//! calls `tick()` from its own event loop; tick returns true when the
//! countdown expired (so the host can invoke the expire callback once).

pub struct CountdownTimer {
    remaining_seconds: i64,
    on_tick: Box<dyn Fn(i64) + Send + Sync>,
    on_expire: Box<dyn Fn() + Send + Sync>,
    finished: bool,
}

impl CountdownTimer {
    pub fn new(timeout_ms: f64, on_tick: Box<dyn Fn(i64) + Send + Sync>, on_expire: Box<dyn Fn() + Send + Sync>) -> Self {
        let remaining_seconds = (timeout_ms / 1000.0).ceil() as i64;
        on_tick(remaining_seconds);
        Self {
            remaining_seconds,
            on_tick,
            on_expire,
            finished: false,
        }
    }

    /// Advance the countdown by one second. Returns true when the timer
    /// expired on this tick (caller should not tick again).
    pub fn tick(&mut self) -> bool {
        if self.finished {
            return true;
        }
        self.remaining_seconds -= 1;
        (self.on_tick)(self.remaining_seconds);
        if self.remaining_seconds <= 0 {
            self.finished = true;
            (self.on_expire)();
            true
        } else {
            false
        }
    }

    pub fn remaining_seconds(&self) -> i64 {
        self.remaining_seconds
    }

    pub fn finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_down_and_expires() {
        let ticks: std::sync::Arc<std::sync::Mutex<Vec<i64>>> = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let expired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ticks_a = ticks.clone();
        let expired_a = expired.clone();
        let mut timer = CountdownTimer::new(
            2500.0,
            Box::new(move |seconds| ticks_a.lock().unwrap().push(seconds)),
            Box::new(move || expired_a.store(true, std::sync::atomic::Ordering::SeqCst)),
        );
        // Initial callback fires with ceil(2.5) = 3.
        assert_eq!(*ticks.lock().unwrap(), vec![3]);
        assert!(!timer.tick());
        assert!(!timer.tick());
        assert!(timer.tick()); // reaches 0
        assert!(expired.load(std::sync::atomic::Ordering::SeqCst));
        assert!(timer.finished());
        assert_eq!(*ticks.lock().unwrap(), vec![3, 2, 1, 0]);
        // Further ticks are no-ops.
        assert!(timer.tick());
        assert_eq!(ticks.lock().unwrap().len(), 4);
    }

    #[test]
    fn exact_second_initial_value() {
        let first = std::sync::Arc::new(std::sync::Mutex::new(0));
        let first_a = first.clone();
        let timer = CountdownTimer::new(
            1000.0,
            Box::new(move |seconds| *first_a.lock().unwrap() = seconds),
            Box::new(|| {}),
        );
        assert_eq!(*first.lock().unwrap(), 1);
        assert_eq!(timer.remaining_seconds(), 1);
    }
}
