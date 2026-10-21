//! Keeps the remapper pads the host plays through seats hidden from games
//! (M22; docs/m22-results.md): another program's virtual pad, DS4Windows's
//! say, which the host chose as one of their controllers.
//!
//! A remapper plugs its pad again whenever its controller reconnects, under
//! the same device id; a pad hidden only once it appears is opened first by
//! any game polling for pads, and HidHide refuses only new opens. So a
//! device stays hidden while its pad is gone, for when it comes back, and
//! while it is back but not yet read again. It comes off as soon as it could
//! be something else's:
//! - plugged in and one of the hosts' own pads, by its serial;
//! - plugged in and no follow's (another program's pad that no controller
//!   of the host's reads, or one in doubt);
//! - gone while a host on this PC is plugging a pad, which may take its id:
//!   looked for every [`GONE_CHECK_EVERY`] while a kept device is gone. A
//!   host plugging faster than that (two hosts on one PC only) may find its
//!   pad hidden until the next look shows it ours.
//!
//! A thread of its own does the hiding, which talks to the driver: the
//! reader tells it each device a follow reads ([`Keeper::news`]), and it
//! looks at what it keeps every [`CHECK_EVERY`]. Everything it hid is shown
//! again when it drops.

use std::{
    any::Any,
    sync::{Arc, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use super::{Backend, VirtualDevice};

/// How often the kept devices are looked at, and a device that couldn't be
/// hidden is tried again.
pub const CHECK_EVERY: Duration = Duration::from_secs(1);
/// How often while a kept device is gone: a host here plugging a pad, which
/// may take its id, does so in moments, from writing its marker to adding
/// the serial (the second review's finding). Only the markers folder and
/// the device tree are read.
pub const GONE_CHECK_EVERY: Duration = Duration::from_millis(100);

enum Msg {
    /// The device a follow reads now, by the follow's place among the
    /// host's controllers.
    Reading(usize, Option<String>),
    Stop,
}

/// Tells the keeper which device a follow reads.
#[derive(Clone)]
pub struct DeviceNews(mpsc::Sender<Msg>);

impl DeviceNews {
    /// Never blocks.
    pub fn reading(&self, place: usize, device: Option<String>) {
        let _ = self.0.send(Msg::Reading(place, device));
    }
}

pub struct Keeper {
    tx: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Keeper {
    /// Hides `devices` at once, each the one the follow at that place will
    /// read (`None`: none plugged in yet), and keeps them hidden. All or
    /// nothing: if one can't be hidden, those hidden are shown again.
    pub fn start(backend: Arc<dyn Backend>, devices: Vec<Option<String>>) -> Result<Self, String> {
        let mut kept = Vec::new();
        for device in devices.iter().flatten() {
            if !kept.iter().any(|(d, _)| d == device) {
                kept.push((device.clone(), backend.hide_virtual(device)?));
            }
        }
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("virtual-hiding".into())
            .spawn(move || run(&*backend, rx, devices, kept))
            .map_err(|e| format!("starting the remapper pads' hiding: {e}"))?;
        Ok(Self {
            tx,
            thread: Some(thread),
        })
    }

    /// Where the follows tell which device they read.
    pub fn news(&self) -> DeviceNews {
        DeviceNews(self.tx.clone())
    }
}

impl Drop for Keeper {
    /// Shows everything again; waits for the driver, so drop it off the
    /// runtime's threads.
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    backend: &dyn Backend,
    rx: mpsc::Receiver<Msg>,
    reading: Vec<Option<String>>,
    mut kept: Vec<(String, Box<dyn Any + Send>)>,
) {
    let mut follows: Vec<Follow> = reading
        .into_iter()
        .map(|device| Follow {
            last: device.clone(),
            now: device,
        })
        .collect();
    // A kept device is gone: looked at more often.
    let mut any_gone = false;
    let mut tried = Instant::now();
    // Devices that couldn't be hidden, said once each.
    let mut warned: Vec<String> = Vec::new();
    loop {
        let wait = if any_gone {
            GONE_CHECK_EVERY
        } else {
            CHECK_EVERY
        };
        let news = match rx.recv_timeout(wait) {
            Ok(Msg::Reading(place, device)) => {
                if let Some(follow) = follows.get_mut(place) {
                    follow.read(device);
                }
                true
            }
            Ok(Msg::Stop) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                any_gone = false;
                kept.retain(|(device, _)| {
                    let its = follows.iter().any(|f| f.its() == Some(device));
                    let now = backend.virtual_device_now(device);
                    let keep = keeps_hidden(its, now);
                    if !keep {
                        tracing::info!(
                            "a remapper pad's device could be something else's now: shown again"
                        );
                    } else if matches!(now, VirtualDevice::Gone { .. }) {
                        any_gone = true;
                    }
                    keep
                });
                false
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // Whatever a follow reads is hidden; one that failed is tried again
        // at the next news, or a second on.
        if !news && tried.elapsed() < CHECK_EVERY {
            continue;
        }
        tried = Instant::now();
        for device in follows.iter().filter_map(|f| f.now.as_ref()) {
            if kept.iter().any(|(d, _)| d == device) {
                continue;
            }
            match backend.hide_virtual(device) {
                Ok(guard) => {
                    tracing::info!("a remapper pad came back as another device: hidden");
                    warned.retain(|d| d != device);
                    kept.push((device.clone(), guard));
                }
                Err(e) if warned.contains(device) => tracing::debug!("hiding a remapper pad: {e}"),
                Err(e) => {
                    tracing::warn!("hiding a remapper pad (tried again every second): {e}");
                    warned.push(device.clone());
                }
            }
        }
    }
    // Shown again here, the guards dropping one by one.
    drop(kept);
}

/// What one follow of the host's reads.
struct Follow {
    now: Option<String>,
    /// The last device it read.
    last: Option<String>,
}

impl Follow {
    fn read(&mut self, device: Option<String>) {
        if device.is_some() {
            self.last = device.clone();
        }
        self.now = device;
    }

    /// The device that is this follow's: the one it reads, or while it
    /// reads none, the last one. A pad plugged again under that device is
    /// the follow's before the reader has it again (its player light, the
    /// reader's next look, up to a second or so), and must stay hidden
    /// meanwhile (the review's finding).
    fn its(&self) -> Option<&String> {
        self.now.as_ref().or(self.last.as_ref())
    }
}

/// Whether a kept device stays hidden, `its` telling whether it is a
/// follow's: plugged in and that follow's, another program's pad or in
/// doubt (while it is plugged in no other pad can take its id); gone, while
/// no host here is plugging a pad. One of the hosts' own, never.
pub fn keeps_hidden(its: bool, now: VirtualDevice) -> bool {
    match now {
        VirtualDevice::Theirs | VirtualDevice::InDoubt => its,
        VirtualDevice::Ours => false,
        VirtualDevice::Gone { hosts_plugging } => !hosts_plugging,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_device_comes_off_once_it_could_be_something_elses() {
        // Read, or gone for a moment (the remapper plugging it again).
        assert!(keeps_hidden(true, VirtualDevice::Theirs));
        assert!(keeps_hidden(
            false,
            VirtualDevice::Gone {
                hosts_plugging: false
            }
        ));
        assert!(keeps_hidden(
            true,
            VirtualDevice::Gone {
                hosts_plugging: false
            }
        ));
        // A host plugging may give its id to one of its pads.
        assert!(!keeps_hidden(
            false,
            VirtualDevice::Gone {
                hosts_plugging: true
            }
        ));
        // One of ours, a follow's or not: never hidden.
        assert!(!keeps_hidden(true, VirtualDevice::Ours));
        assert!(!keeps_hidden(false, VirtualDevice::Ours));
        // In doubt while plugged in: kept if a follow's (no other pad can
        // have its id), not otherwise.
        assert!(keeps_hidden(true, VirtualDevice::InDoubt));
        assert!(!keeps_hidden(false, VirtualDevice::InDoubt));
        // Another program's that is no follow's.
        assert!(!keeps_hidden(false, VirtualDevice::Theirs));
    }

    #[test]
    fn a_follow_reading_nothing_keeps_its_last_device() {
        let mut f = Follow {
            now: Some("01".into()),
            last: Some("01".into()),
        };
        // The pad goes: still the follow's, for when it comes back.
        f.read(None);
        assert_eq!(f.its().map(String::as_str), Some("01"));
        // The follow reads another: that one is its now, not the old.
        f.read(Some("02".into()));
        assert_eq!(f.its().map(String::as_str), Some("02"));
    }
}
