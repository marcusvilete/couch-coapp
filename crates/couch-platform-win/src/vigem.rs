//! ViGEmBus, the virtual gamepad driver (archived upstream at 1.22.0; see
//! docs/architecture.md for why it is still the choice).

/// Connects to ViGEmBus and disconnects again. `Err` explains what is missing.
pub fn probe() -> Result<(), String> {
    vigem_rust::Client::connect()
        .map(drop)
        .map_err(|e| e.to_string())
}
