//! Fakes for tests. Only ever a dev-dependency.

use std::sync::{Arc, Mutex};

use couch_input::{
    HidUsage, InputError, KbmInjector, MouseButton, PadState, Rumble, VirtualPad, VirtualPadFactory,
};

/// One call made on a [`RecordingInjector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbmCall {
    Key(HidUsage, bool),
    MouseMove(i32, i32),
    MouseButton(MouseButton, bool),
    Wheel(i32, i32),
}

/// Records injected keyboard and mouse input instead of sending it to the OS.
#[derive(Debug, Clone, Default)]
pub struct RecordingInjector {
    pub calls: Arc<Mutex<Vec<KbmCall>>>,
}

impl RecordingInjector {
    pub fn calls(&self) -> Vec<KbmCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl KbmInjector for RecordingInjector {
    fn key(&mut self, key: HidUsage, down: bool) -> Result<(), InputError> {
        self.calls.lock().unwrap().push(KbmCall::Key(key, down));
        Ok(())
    }

    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.calls.lock().unwrap().push(KbmCall::MouseMove(dx, dy));
        Ok(())
    }

    fn mouse_button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        self.calls
            .lock()
            .unwrap()
            .push(KbmCall::MouseButton(button, down));
        Ok(())
    }

    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.calls.lock().unwrap().push(KbmCall::Wheel(dx, dy));
        Ok(())
    }
}

/// Records submitted pad states and replays queued rumble.
#[derive(Debug, Clone, Default)]
pub struct RecordingPad {
    pub states: Arc<Mutex<Vec<PadState>>>,
    pub rumble: Arc<Mutex<Option<Rumble>>>,
}

impl RecordingPad {
    pub fn states(&self) -> Vec<PadState> {
        self.states.lock().unwrap().clone()
    }

    /// Makes the next [`VirtualPad::take_rumble`] return `rumble`, as if a game sent it.
    pub fn queue_rumble(&self, rumble: Rumble) {
        *self.rumble.lock().unwrap() = Some(rumble);
    }
}

impl VirtualPad for RecordingPad {
    fn submit(&mut self, state: &PadState) -> Result<(), InputError> {
        self.states.lock().unwrap().push(*state);
        Ok(())
    }

    fn take_rumble(&mut self) -> Option<Rumble> {
        self.rumble.lock().unwrap().take()
    }
}

/// Plugs [`RecordingPad`]s and keeps a handle to each for assertions.
#[derive(Debug, Clone, Default)]
pub struct RecordingPadFactory {
    pub plugged: Arc<Mutex<Vec<RecordingPad>>>,
}

impl VirtualPadFactory for RecordingPadFactory {
    fn plug(&self) -> Result<Box<dyn VirtualPad>, InputError> {
        let pad = RecordingPad::default();
        self.plugged.lock().unwrap().push(pad.clone());
        Ok(Box::new(pad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_records_in_order() {
        let mut injector = RecordingInjector::default();
        injector.key(HidUsage(0x04), true).unwrap();
        injector.mouse_move(3, -2).unwrap();
        assert_eq!(
            injector.calls(),
            vec![
                KbmCall::Key(HidUsage(0x04), true),
                KbmCall::MouseMove(3, -2)
            ]
        );
    }

    #[test]
    fn factory_keeps_handles_to_plugged_pads() {
        let factory = RecordingPadFactory::default();
        let mut pad = factory.plug().unwrap();
        pad.submit(&PadState::NEUTRAL).unwrap();
        let plugged = factory.plugged.lock().unwrap();
        assert_eq!(plugged[0].states(), vec![PadState::NEUTRAL]);
    }
}
