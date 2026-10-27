//! ViGEmBus, the virtual gamepad driver (archived upstream at 1.22.0; see
//! docs/architecture.md for why it is still the choice). Each pad is a
//! virtual Xbox 360 controller, which every PC game understands through
//! XInput.

use std::time::{Duration, Instant};

use couch_input::{
    InputError, PAD_SETUP_LIMIT, PadState, PendingPad, Plugged, Rumble, VirtualPad,
    VirtualPadFactory,
};
use vigem_rust::{
    Client, ClientError, NotificationReceiver, Pending, SerialDiscovery, TargetHandle, X360Button,
    X360Notification, X360Report, Xbox360,
};

use crate::virtual_pads::{self, PadMarker};

/// Connects to ViGEmBus and disconnects again. `Err` explains what is missing.
pub fn probe() -> Result<(), String> {
    Client::connect().map(drop).map_err(|e| e.to_string())
}

fn driver(e: impl std::fmt::Display) -> InputError {
    InputError::DriverUnavailable(e.to_string())
}

/// Plugs virtual Xbox 360 pads.
pub struct VigemPadFactory {
    client: Client,
    ids: Option<(u16, u16)>,
    /// Each pad leaves a marker (`virtual_pads`); only the probe's don't.
    marked: bool,
}

impl VigemPadFactory {
    /// `ids` overrides the USB vendor and product id. `None` keeps a real
    /// Xbox 360 controller's, which games and Steam need to recognise it
    /// (with others only the D-pad works). Only CLI test tools pass
    /// [`couch_input::DEV_PAD_IDS`]. Either way a stream window on this PC
    /// skips it while it is plugged in (`virtual_pads`).
    pub fn connect(ids: Option<(u16, u16)>) -> Result<Self, InputError> {
        // From serial 1, as the original ViGEm client: each serial is a device
        // of its own to Windows, set up the first time it is plugged, so a PC
        // sets up the first few once (P10). Looked up first, so no plug is
        // refused (each refusal costs ViGEmBus a little kernel memory).
        let client = Client::builder()
            .first_serial(1)
            .serial_discovery(SerialDiscovery::Eager)
            .connect()
            .map_err(driver)?;
        Ok(Self {
            client,
            ids,
            marked: true,
        })
    }

    /// Pads that leave no marker, standing in for another program's ViGEm
    /// pads (a remapper's), for `couch-cli pad-origins` only (P9). Never for
    /// hosting: a stream window on this PC would read them and send them
    /// back.
    pub fn connect_unmarked(ids: Option<(u16, u16)>) -> Result<Self, InputError> {
        Ok(Self {
            marked: false,
            ..Self::connect(ids)?
        })
    }
}

impl VirtualPadFactory for VigemPadFactory {
    fn plug(&self) -> Result<Box<dyn VirtualPad>, InputError> {
        self.plug_pending()?.wait(PAD_SETUP_LIMIT)
    }

    fn plug_pending(&self) -> Result<Plugged, InputError> {
        let mut pending = self.plug_start()?;
        Ok(match pending.ready(FIRST_WAIT)? {
            Some(pad) => Plugged::Ready(Box::new(pad)),
            None => {
                tracing::info!(
                    "virtual pad {}: not taking input yet; Windows may still be setting it up",
                    pending.serial
                );
                Plugged::SettingUp(Box::new(pending))
            }
        })
    }
}

/// How long a pad gets to come up before it counts as still being set up: a
/// pad whose device Windows already knows takes well under a second.
const FIRST_WAIT: Duration = Duration::from_secs(3);

/// A pad plugged for `couch-cli pad-origins` (P9), with what ViGEm says of
/// it. Unplugs when dropped.
pub struct ProbePad {
    pad: VigemPad,
    light: Option<u8>,
}

impl VigemPadFactory {
    pub fn plug_for_probe(&self) -> Result<ProbePad, InputError> {
        let mut pending = self.plug_start()?;
        let until = Instant::now() + PAD_SETUP_LIMIT;
        let pad = loop {
            if let Some(pad) = pending.ready(Duration::from_millis(500))? {
                break pad;
            }
            if Instant::now() >= until {
                return Err(driver("the virtual pad wasn't ready in time"));
            }
        };
        Ok(ProbePad { pad, light: None })
    }

    /// Plugs a pad, marked as ours before it appears and given its serial at
    /// once (`virtual_pads`), not yet waiting for it to take input.
    fn plug_start(&self) -> Result<VigemPending, InputError> {
        let mut builder = self.client.new_x360_target();
        if let Some((vid, pid)) = self.ids {
            builder = builder.with_vid(vid).with_pid(pid);
        }
        // Marked before it appears, so a stream window on this PC never
        // opens it (see `virtual_pads`), and given its serial as soon as it
        // has one, before it takes input: Windows may take minutes to set a
        // new pad up (P10), and until the serial is there every ViGEm pad
        // counts as ours. In this process the mark can't fail (M22: the
        // host's reader must never take its own pad for a remapper's). A
        // failed plug drops the marker.
        let marker = self.marked.then(virtual_pads::mark);
        let handle = builder.plug().map_err(driver)?;
        let serial = handle.serial_no();
        if let Some(marker) = &marker {
            // Without it the marker stays serial-less: every ViGEm pad then
            // counts as ours, as before P9.
            if let Err(e) = marker.set_serial(serial) {
                tracing::warn!("recording the virtual pad's serial: {e}");
            }
        }
        Ok(VigemPending {
            client: self.client.clone(),
            handle: Some(handle),
            marker,
            serial,
            said: None,
        })
    }
}

/// A pad plugged in that doesn't take input yet (P10). Dropping it unplugs
/// it (vigem-rust's handle), then removes its marker: until then it stays
/// known as ours.
struct VigemPending {
    client: Client,
    handle: Option<TargetHandle<Xbox360, Pending>>,
    marker: Option<PadMarker>,
    serial: u32,
    /// The last error logged, so each is logged once.
    said: Option<String>,
}

impl VigemPending {
    /// Waits up to `wait` for the pad: `Ok(None)` while it doesn't take
    /// input, `Err` once it's gone for good.
    fn ready(&mut self, wait: Duration) -> Result<Option<VigemPad>, InputError> {
        let started = Instant::now();
        let Some(handle) = &self.handle else {
            return Err(driver("the virtual pad was already handed over"));
        };
        let error = match handle.wait_for_ready_timeout(wait) {
            Ok(pad) => {
                let rumble = pad.register_notification().map_err(driver)?;
                // The ready handle shares the pending one's pad: dropping the
                // pending one doesn't unplug it.
                self.handle = None;
                return Ok(Some(VigemPad {
                    _client: self.client.clone(),
                    pad,
                    rumble,
                    last_rumble: Rumble::default(),
                    _marker: self.marker.take(),
                }));
            }
            Err(ClientError::ReadyTimeout { .. }) => return Ok(None),
            Err(
                e @ (ClientError::TargetDoesNotExist { .. }
                | ClientError::ClientNoLongerExists
                | ClientError::Bus(
                    vigem_rust::BusError::DeviceGone { .. }
                    | vigem_rust::BusError::BusNotFound { .. }
                    | vigem_rust::BusError::AccessDenied { .. },
                )),
            ) => return Err(driver(e)),
            Err(e) => e.to_string(),
        };
        // Anything else may be Windows still setting it up: keep waiting,
        // saying so once per kind of answer.
        if self.said.as_deref() != Some(error.as_str()) {
            tracing::info!("virtual pad {}: not ready yet ({error})", self.serial);
            self.said = Some(error);
        }
        std::thread::sleep(wait.saturating_sub(started.elapsed()));
        Ok(None)
    }
}

impl PendingPad for VigemPending {
    fn poll(&mut self, wait: Duration) -> Result<Option<Box<dyn VirtualPad>>, InputError> {
        Ok(self
            .ready(wait)?
            .map(|pad| Box::new(pad) as Box<dyn VirtualPad>))
    }

    fn give_up(mut self: Box<Self>) -> Box<dyn Send> {
        tracing::info!("virtual pad {}: given up, unplugging it", self.serial);
        // Unplugged now; the marker stays with the caller.
        self.handle = None;
        Box::new(self.marker.take())
    }
}

impl Drop for VigemPending {
    fn drop(&mut self) {
        if self.handle.is_some() {
            tracing::info!("virtual pad {}: given up, unplugging it", self.serial);
        }
        // The handle first (it unplugs the pad), then the marker.
        self.handle = None;
    }
}

impl ProbePad {
    /// Its ViGEm serial: the last part of its device's instance id.
    pub fn serial(&self) -> u32 {
        self.pad.pad.serial_no()
    }

    /// The XInput slot ViGEm reports for it.
    pub fn user_index(&self) -> Option<u32> {
        self.pad.pad.user_index().ok()
    }

    /// The last player light ViGEm passed on for it, if any came yet.
    pub fn light(&mut self) -> Option<u8> {
        while let Ok(n) = self.pad.rumble.try_recv() {
            self.light = Some(n.led_number);
        }
        self.light
    }
}

/// Unplugs when dropped.
struct VigemPad {
    /// The pad leaves the bus when the last client handle goes.
    _client: Client,
    pad: TargetHandle<Xbox360>,
    rumble: NotificationReceiver<X360Notification>,
    last_rumble: Rumble,
    /// Removed after the pad has gone (fields drop in order).
    _marker: Option<PadMarker>,
}

impl VirtualPad for VigemPad {
    fn submit(&mut self, state: &PadState) -> Result<(), InputError> {
        let report = X360Report {
            buttons: X360Button::from_bits_truncate(state.buttons.bits()),
            left_trigger: state.left_trigger,
            right_trigger: state.right_trigger,
            thumb_lx: state.left_stick.0,
            thumb_ly: state.left_stick.1,
            thumb_rx: state.right_stick.0,
            thumb_ry: state.right_stick.1,
        };
        self.pad
            .update(&report)
            .map_err(|e| InputError::Injection(e.to_string()))
    }

    fn take_rumble(&mut self) -> Option<Rumble> {
        let mut latest = None;
        while let Ok(n) = self.rumble.try_recv() {
            latest = Some(Rumble {
                low_frequency: n.large_motor,
                high_frequency: n.small_motor,
            });
        }
        let latest = latest.filter(|r| *r != self.last_rumble)?;
        self.last_rumble = latest;
        Some(latest)
    }
}
