//! What happens when the captured window goes: follow the game to a new
//! window, end with "the game exited", or end with "the source was lost".
//!
//! Sans-IO: the caller reports the window gone, then asks [`SourceWatch::check`]
//! every so often with what it sees (is the game's process running, which of
//! its windows could replace the old one) and does what it says.
//!
//! - The game's process exited: [`Verdict::GameExited`], at once.
//! - It runs and shows a usable window: [`Verdict::Follow`] that window.
//! - Neither within the grace: [`Verdict::SourceLost`].
//! - Without a process to watch (a screen, or a process we may not open):
//!   [`Verdict::SourceLost`] at once.

use std::time::{Duration, Instant};

use super::NativeId;

/// How long a game may take to show a new window before the stream ends.
pub const REPLACE_GRACE: Duration = Duration::from_secs(5);
/// How often to look for the new window during the grace.
pub const REPLACE_POLL: Duration = Duration::from_millis(250);

/// What the game's process looks like right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Game {
    /// Nothing to watch.
    Unknown,
    Exited,
    /// Running, with these candidate windows, best first.
    Running(Vec<NativeId>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Look again in [`REPLACE_POLL`].
    Wait,
    /// Capture this window from now on.
    Follow(NativeId),
    GameExited,
    SourceLost,
}

#[derive(Debug, Clone)]
pub struct SourceWatch {
    grace: Duration,
    lost_since: Option<Instant>,
    ended: bool,
}

impl SourceWatch {
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            lost_since: None,
            ended: false,
        }
    }

    /// The captured window is gone. Repeated reports keep the first time.
    pub fn lost(&mut self, now: Instant) {
        if !self.ended && self.lost_since.is_none() {
            self.lost_since = Some(now);
        }
    }

    /// Whether a window is gone and its replacement is being looked for.
    pub fn looking(&self) -> bool {
        self.lost_since.is_some()
    }

    /// Whether the watch ended the stream already.
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// What to do, given the game as it is now. Only meaningful while
    /// [`looking`](Self::looking); otherwise [`Verdict::Wait`].
    pub fn check(&mut self, game: &Game, now: Instant) -> Verdict {
        let Some(since) = self.lost_since else {
            return Verdict::Wait;
        };
        let verdict = match game {
            Game::Exited => Verdict::GameExited,
            Game::Unknown => Verdict::SourceLost,
            Game::Running(windows) => match windows.first() {
                Some(&window) => Verdict::Follow(window),
                None if now.duration_since(since) >= self.grace => Verdict::SourceLost,
                None => Verdict::Wait,
            },
        };
        match verdict {
            Verdict::Wait => {}
            Verdict::Follow(_) => self.lost_since = None,
            Verdict::GameExited | Verdict::SourceLost => {
                self.lost_since = None;
                self.ended = true;
            }
        }
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(5);

    fn lost_at(t: Instant) -> SourceWatch {
        let mut watch = SourceWatch::new(GRACE);
        watch.lost(t);
        watch
    }

    #[test]
    fn nothing_to_do_while_the_window_is_there() {
        let mut watch = SourceWatch::new(GRACE);
        assert!(!watch.looking());
        assert_eq!(
            watch.check(&Game::Exited, Instant::now()),
            Verdict::Wait,
            "an exited game is only acted on once its window is gone"
        );
    }

    #[test]
    fn an_exited_game_ends_at_once() {
        let t = Instant::now();
        let mut watch = lost_at(t);
        assert_eq!(watch.check(&Game::Exited, t), Verdict::GameExited);
        assert!(watch.ended());
        assert_eq!(watch.check(&Game::Exited, t), Verdict::Wait, "only once");
    }

    #[test]
    fn a_new_window_is_followed() {
        let t = Instant::now();
        let mut watch = lost_at(t);
        assert_eq!(
            watch.check(&Game::Running(vec![]), t + Duration::from_secs(1)),
            Verdict::Wait
        );
        assert_eq!(
            watch.check(&Game::Running(vec![7, 9]), t + Duration::from_secs(2)),
            Verdict::Follow(7)
        );
        assert!(!watch.looking() && !watch.ended());
        // The new window can go too, and the grace starts again.
        let later = t + Duration::from_secs(60);
        watch.lost(later);
        assert_eq!(
            watch.check(
                &Game::Running(vec![]),
                later + GRACE - Duration::from_millis(1)
            ),
            Verdict::Wait
        );
        assert_eq!(
            watch.check(&Game::Running(vec![]), later + GRACE),
            Verdict::SourceLost
        );
    }

    #[test]
    fn a_running_game_without_a_window_is_lost_after_the_grace() {
        let t = Instant::now();
        let mut watch = lost_at(t);
        assert_eq!(
            watch.check(&Game::Running(vec![]), t + GRACE / 2),
            Verdict::Wait
        );
        assert_eq!(
            watch.check(&Game::Running(vec![]), t + GRACE),
            Verdict::SourceLost
        );
        assert!(watch.ended());
    }

    #[test]
    fn the_game_exiting_during_the_grace_is_game_exited() {
        let t = Instant::now();
        let mut watch = lost_at(t);
        assert_eq!(watch.check(&Game::Running(vec![]), t), Verdict::Wait);
        assert_eq!(
            watch.check(&Game::Exited, t + Duration::from_secs(3)),
            Verdict::GameExited
        );
    }

    #[test]
    fn nothing_to_watch_is_lost_at_once() {
        let t = Instant::now();
        let mut watch = lost_at(t);
        assert_eq!(watch.check(&Game::Unknown, t), Verdict::SourceLost);
    }

    #[test]
    fn repeated_reports_keep_the_first_time() {
        let t = Instant::now();
        let mut watch = lost_at(t);
        watch.lost(t + Duration::from_secs(4));
        assert_eq!(
            watch.check(&Game::Running(vec![]), t + GRACE),
            Verdict::SourceLost
        );
        // And nothing restarts it once it ended.
        watch.lost(t + Duration::from_secs(10));
        assert!(!watch.looking());
    }
}
