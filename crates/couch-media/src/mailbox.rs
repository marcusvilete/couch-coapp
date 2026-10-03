//! A single-slot handoff where a newer value replaces an older one.
//!
//! Video wants the latest frame, not every frame: a consumer that falls behind
//! should skip ahead, not build a queue that turns into latency.

use std::{
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

pub struct Mailbox<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}

struct State<T> {
    value: Option<T>,
    closed: bool,
    replaced: u64,
}

impl<T> Mailbox<T> {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                value: None,
                closed: false,
                replaced: 0,
            }),
            ready: Condvar::new(),
        }
    }

    /// Stores `value`, dropping one the consumer hasn't taken yet. Returns
    /// `true` if it replaced such a value.
    pub fn put(&self, value: T) -> bool {
        let mut state = self.state.lock().unwrap();
        let replaced = state.value.replace(value).is_some();
        if replaced {
            state.replaced += 1;
        }
        drop(state);
        self.ready.notify_one();
        replaced
    }

    /// No more values will come. A value already waiting can still be taken.
    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.ready.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    /// Takes the waiting value, if any, without blocking.
    pub fn try_take(&self) -> Option<T> {
        self.state.lock().unwrap().value.take()
    }

    /// Waits up to `timeout` for a value. `Ok(None)` means the timeout passed;
    /// `Err(Closed)` means the mailbox is closed and empty.
    pub fn take_timeout(&self, timeout: Duration) -> Result<Option<T>, Closed> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(value) = state.value.take() {
                return Ok(Some(value));
            }
            if state.closed {
                return Err(Closed);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            state = self.ready.wait_timeout(state, deadline - now).unwrap().0;
        }
    }

    /// How many values were dropped because a newer one replaced them.
    pub fn replaced_count(&self) -> u64 {
        self.state.lock().unwrap().replaced
    }
}

impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use super::*;

    #[test]
    fn newer_values_replace_older_ones() {
        let mailbox = Mailbox::new();
        assert!(!mailbox.put(1));
        assert!(mailbox.put(2));
        assert_eq!(mailbox.try_take(), Some(2));
        assert_eq!(mailbox.try_take(), None);
        assert_eq!(mailbox.replaced_count(), 1);
    }

    #[test]
    fn take_waits_for_a_value_from_another_thread() {
        let mailbox = Arc::new(Mailbox::new());
        let producer = {
            let mailbox = mailbox.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                mailbox.put("frame");
            })
        };
        assert_eq!(
            mailbox.take_timeout(Duration::from_secs(5)),
            Ok(Some("frame"))
        );
        producer.join().unwrap();
    }

    #[test]
    fn close_drains_then_reports_closed() {
        let mailbox = Mailbox::new();
        mailbox.put(7);
        mailbox.close();
        assert_eq!(mailbox.take_timeout(Duration::ZERO), Ok(Some(7)));
        assert_eq!(mailbox.take_timeout(Duration::ZERO), Err(Closed));
    }

    #[test]
    fn timeout_without_a_value() {
        let mailbox: Mailbox<u8> = Mailbox::new();
        assert_eq!(mailbox.take_timeout(Duration::from_millis(5)), Ok(None));
    }
}
