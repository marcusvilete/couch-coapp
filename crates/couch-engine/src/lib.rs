//! The engine the UI talks to: [`Command`]s in, [`Event`]s out.
//!
//! It knows nothing about Tauri, so the shell stays swappable and the engine
//! can be driven from tests. The command and event types are exported to
//! TypeScript with ts-rs (`cargo test -p couch-engine` regenerates
//! `apps/desktop/src/bindings`).

mod profile;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

pub use profile::{Profile, ProfileError};

/// Everything the UI can ask of the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum Command {
    /// Facts about this install and build.
    GetInfo,
    /// A round trip through the engine; also announced as [`Event::Pong`].
    Ping { nonce: u32 },
}

/// The reply to a [`Command`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum Response {
    Info(EngineInfo),
    Pong { nonce: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EngineInfo {
    pub app_version: String,
    pub protocol: String,
    pub profile: String,
    pub data_dir: String,
}

/// Things the engine announces on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum Event {
    Pong { nonce: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {}

pub struct Engine {
    profile: Profile,
    events: broadcast::Sender<Event>,
}

impl Engine {
    pub fn new(profile: Profile) -> Self {
        let (events, _) = broadcast::channel(256);
        Self { profile, events }
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// A new subscription to engine events. Slow subscribers miss events
    /// rather than holding the engine up.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn handle(&self, command: Command) -> Result<Response, EngineError> {
        match command {
            Command::GetInfo => Ok(Response::Info(EngineInfo {
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: String::from_utf8_lossy(couch_proto::ALPN).into_owned(),
                profile: self.profile.name().to_string(),
                data_dir: self.profile.data_dir().display().to_string(),
            })),
            Command::Ping { nonce } => {
                // No subscribers is fine: the UI may not be listening yet.
                let _ = self.events.send(Event::Pong { nonce });
                Ok(Response::Pong { nonce })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        Engine::new(Profile::new("test", std::env::temp_dir().join("couch-test")).unwrap())
    }

    #[tokio::test]
    async fn ping_replies_and_announces() {
        let engine = engine();
        let mut events = engine.subscribe();
        let reply = engine.handle(Command::Ping { nonce: 7 }).await.unwrap();
        assert_eq!(reply, Response::Pong { nonce: 7 });
        assert_eq!(events.recv().await.unwrap(), Event::Pong { nonce: 7 });
    }

    #[tokio::test]
    async fn info_names_the_profile() {
        let Response::Info(info) = engine().handle(Command::GetInfo).await.unwrap() else {
            panic!("expected info");
        };
        assert_eq!(info.profile, "test");
        assert_eq!(info.protocol, "couch-coapp/0");
    }

    /// The TypeScript bindings assume this JSON shape.
    #[test]
    fn commands_and_responses_use_tagged_json() {
        let command: Command = serde_json::from_str(r#"{"type":"ping","nonce":1}"#).unwrap();
        assert_eq!(command, Command::Ping { nonce: 1 });
        let get_info: Command = serde_json::from_str(r#"{"type":"getInfo"}"#).unwrap();
        assert_eq!(get_info, Command::GetInfo);
        let reply = serde_json::to_string(&Response::Pong { nonce: 1 }).unwrap();
        assert_eq!(reply, r#"{"type":"pong","nonce":1}"#);
    }
}
