//! ViGEmBus, the virtual gamepad driver (archived upstream at 1.22.0; see
//! docs/architecture.md for why it is still the choice). Each pad is a
//! virtual Xbox 360 controller, which every PC game understands through
//! XInput.

use couch_input::{InputError, PadState, Rumble, VirtualPad, VirtualPadFactory};
use vigem_rust::{
    Client, NotificationReceiver, TargetHandle, X360Button, X360Notification, X360Report, Xbox360,
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
        Ok(Self {
            client: Client::connect().map_err(driver)?,
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
        Ok(Box::new(self.plug_pad()?))
    }
}

/// A pad plugged for `couch-cli pad-origins` (P9), with what ViGEm says of
/// it. Unplugs when dropped.
pub struct ProbePad {
    pad: VigemPad,
    light: Option<u8>,
}

impl VigemPadFactory {
    pub fn plug_for_probe(&self) -> Result<ProbePad, InputError> {
        Ok(ProbePad {
            pad: self.plug_pad()?,
            light: None,
        })
    }

    fn plug_pad(&self) -> Result<VigemPad, InputError> {
        let mut builder = self.client.new_x360_target();
        if let Some((vid, pid)) = self.ids {
            builder = builder.with_vid(vid).with_pid(pid);
        }
        // Marked before it appears, so a stream window on this PC never
        // opens it (see `virtual_pads`), and given its serial once it has
        // one; until then every ViGEm pad counts as ours. In this process
        // the mark can't fail (M22: the host's reader must never take its
        // own pad for a remapper's). A failed plug drops the marker.
        let marker = self.marked.then(virtual_pads::mark);
        let pad = builder
            .plug()
            .map_err(driver)?
            .wait_for_ready()
            .map_err(driver)?;
        if let Some(marker) = &marker {
            // Without it the marker stays serial-less: every ViGEm pad then
            // counts as ours, as before P9.
            if let Err(e) = marker.set_serial(pad.serial_no()) {
                tracing::warn!("recording the virtual pad's serial: {e}");
            }
        }
        let rumble = pad.register_notification().map_err(driver)?;
        Ok(VigemPad {
            _client: self.client.clone(),
            pad,
            rumble,
            last_rumble: Rumble::default(),
            _marker: marker,
        })
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
