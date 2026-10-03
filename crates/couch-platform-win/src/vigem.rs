//! ViGEmBus, the virtual gamepad driver (archived upstream at 1.22.0; see
//! docs/architecture.md for why it is still the choice). Each pad is a
//! virtual Xbox 360 controller, which every PC game understands through
//! XInput.

use couch_input::{InputError, PadState, Rumble, VirtualPad, VirtualPadFactory};
use vigem_rust::{
    Client, NotificationReceiver, TargetHandle, X360Button, X360Notification, X360Report, Xbox360,
};

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
}

impl VigemPadFactory {
    /// `ids` overrides the USB vendor and product id; development builds use
    /// [`couch_input::DEV_PAD_IDS`] so a local viewer can skip the pad.
    pub fn connect(ids: Option<(u16, u16)>) -> Result<Self, InputError> {
        Ok(Self {
            client: Client::connect().map_err(driver)?,
            ids,
        })
    }
}

impl VirtualPadFactory for VigemPadFactory {
    fn plug(&self) -> Result<Box<dyn VirtualPad>, InputError> {
        let mut builder = self.client.new_x360_target();
        if let Some((vid, pid)) = self.ids {
            builder = builder.with_vid(vid).with_pid(pid);
        }
        let pad = builder
            .plug()
            .map_err(driver)?
            .wait_for_ready()
            .map_err(driver)?;
        let rumble = pad.register_notification().map_err(driver)?;
        Ok(Box::new(VigemPad {
            _client: self.client.clone(),
            pad,
            rumble,
            last_rumble: Rumble::default(),
        }))
    }
}

/// Unplugs when dropped.
struct VigemPad {
    /// The pad leaves the bus when the last client handle goes.
    _client: Client,
    pad: TargetHandle<Xbox360>,
    rumble: NotificationReceiver<X360Notification>,
    last_rumble: Rumble,
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
