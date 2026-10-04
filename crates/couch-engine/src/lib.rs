//! The engine the UI talks to: [`Command`]s in, [`Event`]s out.
//!
//! It knows nothing about Tauri, so the shell stays swappable and the engine
//! can be driven from tests. The command and event types are exported to
//! TypeScript with ts-rs (`cargo test -p couch-engine` regenerates
//! `apps/desktop/src/bindings`).
//!
//! Rooms: [`rooms`] runs each room's state machine (`couch_room::room`) over
//! iroh. Identity and remembered rooms live in the profile's folder
//! ([`store`]), sealed with DPAPI on Windows ([`secrets`]).

mod profile;
mod rooms;
pub mod secrets;
mod store;

use std::{fmt, sync::Arc, time::Duration};

use couch_net::endpoint::RelayChoice;
use couch_proto::invite::InviteError;
use couch_room::{
    limits::LimitConfig,
    room::{RoomError, Timings},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use ts_rs::TS;

pub use profile::{Profile, ProfileError};
pub use secrets::SecretStore;
pub use store::StoreError;

/// Everything the UI can ask of the engine. Its `Debug` leaves out passwords
/// and invites.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum Command {
    /// Facts about this install and build.
    GetInfo,
    /// A round trip through the engine; also announced as [`Event::Pong`].
    Ping { nonce: u32 },
    /// This profile's display name and member id.
    GetProfile,
    /// Shown to the other members of every room.
    SetDisplayName { name: String },
    /// Every remembered room.
    ListRooms,
    /// A new room owned by this profile. Only the owner lets people in.
    CreateRoom { name: String, password: String },
    /// Joins with an invite link (or its bare payload) and the password.
    /// The room's owner must be online.
    JoinRoom { invite: String, password: String },
    /// Owner only: a new invite link, good for a week (or until revoked, or
    /// five wrong passwords). With relays configured it names our relay,
    /// never our IP address, so right after startup it may wait a few
    /// seconds for the relay.
    GetInvite { room: String },
    /// Owner only: every invite made so far stops working.
    RevokeInvites { room: String },
    /// Leaves for good: the others are told, and the room is forgotten. The
    /// owner leaving closes the room for everyone.
    LeaveRoom { room: String },
    /// Owner only: takes a member out of the room for good, and revokes
    /// every invite (the banned member may hold one).
    BanMember { room: String, member: String },
}

/// The reply to a [`Command`]. Its `Debug` leaves out invite links.
#[derive(Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum Response {
    Info(EngineInfo),
    Pong { nonce: u32 },
    Profile(ProfileView),
    Rooms { rooms: Vec<RoomView> },
    Room(RoomView),
    Invite { link: String },
    Done,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GetInfo => f.write_str("GetInfo"),
            Self::Ping { nonce } => f.debug_struct("Ping").field("nonce", nonce).finish(),
            Self::GetProfile => f.write_str("GetProfile"),
            Self::SetDisplayName { name } => f
                .debug_struct("SetDisplayName")
                .field("name", name)
                .finish(),
            Self::ListRooms => f.write_str("ListRooms"),
            Self::CreateRoom { name, .. } => f
                .debug_struct("CreateRoom")
                .field("name", name)
                .finish_non_exhaustive(),
            // The invite holds the token that, with the password, lets one in.
            Self::JoinRoom { .. } => f.write_str("JoinRoom { .. }"),
            Self::GetInvite { room } => f.debug_struct("GetInvite").field("room", room).finish(),
            Self::RevokeInvites { room } => {
                f.debug_struct("RevokeInvites").field("room", room).finish()
            }
            Self::LeaveRoom { room } => f.debug_struct("LeaveRoom").field("room", room).finish(),
            Self::BanMember { room, member } => f
                .debug_struct("BanMember")
                .field("room", room)
                .field("member", member)
                .finish(),
        }
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info(info) => f.debug_tuple("Info").field(info).finish(),
            Self::Pong { nonce } => f.debug_struct("Pong").field("nonce", nonce).finish(),
            Self::Profile(profile) => f.debug_tuple("Profile").field(profile).finish(),
            Self::Rooms { rooms } => f.debug_struct("Rooms").field("rooms", rooms).finish(),
            Self::Room(room) => f.debug_tuple("Room").field(room).finish(),
            Self::Invite { .. } => f.write_str("Invite { .. }"),
            Self::Done => f.write_str("Done"),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfileView {
    pub display_name: String,
    /// Our member id (the endpoint id), lowercase hex.
    pub member_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RoomView {
    /// Lowercase hex.
    pub id: String,
    pub name: String,
    pub is_owner: bool,
    /// This profile first, then by name.
    pub members: Vec<MemberView>,
    /// For the owner: while set, newcomers can't join, after too many wrong
    /// passwords. Seconds left.
    pub joins_paused_for_secs: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MemberView {
    pub id: String,
    pub name: String,
    pub is_owner: bool,
    pub link: MemberLink,
}

/// How a member is connected to us.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum MemberLink {
    Me,
    /// A direct peer-to-peer path.
    Direct,
    /// Through a relay server: works, with more latency.
    Relay,
    /// Online and connected to others, but not to us (yet).
    Online,
    Offline,
}

/// Why a room went away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum RoomGone {
    Left,
    Banned,
    /// The owner closed the room, or took us off its member list.
    Removed,
}

/// Things the engine announces on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum Event {
    Pong {
        nonce: u32,
    },
    /// A room's name, members or connections changed.
    RoomChanged {
        room: RoomView,
    },
    RoomRemoved {
        room: String,
        reason: RoomGone,
    },
    /// Too many wrong passwords: newcomers can't join our room for a while.
    JoinsPaused {
        room: String,
        minutes: u32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("could not start networking: {0}")]
    Net(String),
    #[error(transparent)]
    Invite(#[from] InviteError),
    #[error("wrong password")]
    WrongPassword,
    /// Someone in the room refused us; the text says why.
    #[error("{0}")]
    Refused(&'static str),
    #[error(
        "the room's owner isn't online; invites work while the owner's Couch Co-App is running"
    )]
    OwnerOffline,
    #[error("passwords can't be empty")]
    EmptyPassword,
    #[error("no such room")]
    UnknownRoom,
    #[error("no such member")]
    UnknownMember,
    #[error(transparent)]
    Room(#[from] RoomError),
    /// Invites name our relay rather than our IP address, and none is
    /// reachable yet.
    #[error("not connected to the relay yet; check the internet connection and try again")]
    NotOnline,
    #[error("the system's random number source failed: {0}")]
    Random(String),
    #[error("the engine has stopped")]
    Stopped,
}

/// How the engine runs. Tests shorten the timings and turn relays off.
#[derive(Clone)]
pub struct EngineOptions {
    pub relays: RelayChoice,
    pub secret_store: Arc<dyn SecretStore>,
    pub limits: LimitConfig,
    pub timings: Timings,
    /// How often rooms get a chance to dial and repair.
    pub tick: Duration,
    /// How long an invite works.
    pub invite_valid_for: Duration,
    /// A fixed UDP port instead of a random one. Members find each other
    /// again after restarts through the relay; without one (a LAN, the
    /// tests) only a fixed port keeps saved addresses valid.
    pub port: Option<u16>,
}

impl EngineOptions {
    /// The desktop app until our own relay exists (M7): iroh's public relays,
    /// which are for development only.
    pub fn development() -> Self {
        Self {
            relays: RelayChoice::N0Public,
            secret_store: secrets::platform_store(),
            limits: LimitConfig::default(),
            timings: Timings::default(),
            tick: Duration::from_millis(200),
            invite_valid_for: Duration::from_secs(7 * 24 * 3600),
            port: None,
        }
    }
}

pub struct Engine {
    profile: Profile,
    member_id: String,
    events: broadcast::Sender<Event>,
    actor: mpsc::Sender<rooms::Input>,
}

impl Engine {
    /// Loads (or creates) the profile's identity, binds the endpoint and
    /// reconnects every remembered room.
    pub async fn start(profile: Profile, options: EngineOptions) -> Result<Self, EngineError> {
        let (events, _) = broadcast::channel(256);
        let started = rooms::start(&profile, options, events.clone()).await?;
        Ok(Self {
            profile,
            member_id: started.member_id.to_hex(),
            events,
            actor: started.inbox,
        })
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Our member id, lowercase hex.
    pub fn member_id(&self) -> &str {
        &self.member_id
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
            command => {
                let (reply, response) = oneshot::channel();
                self.actor
                    .send(rooms::Input::Command(command, reply))
                    .await
                    .map_err(|_| EngineError::Stopped)?;
                response.await.map_err(|_| EngineError::Stopped)?
            }
        }
    }

    /// Tells every connected member we are going, then closes the endpoint.
    pub async fn shutdown(&self) {
        let (done, finished) = oneshot::channel();
        if self.actor.send(rooms::Input::Shutdown(done)).await.is_ok() {
            let _ = finished.await;
        }
    }

    /// For tests: per member of `room`, whether a connection is up and
    /// whether we dialed it.
    #[doc(hidden)]
    pub async fn connections(&self, room: &str) -> Vec<(String, bool)> {
        let (reply, response) = oneshot::channel();
        if self
            .actor
            .send(rooms::Input::Connections(room.to_string(), reply))
            .await
            .is_err()
        {
            return Vec::new();
        }
        response.await.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn engine(dir: &std::path::Path) -> Engine {
        let options = EngineOptions {
            relays: RelayChoice::Disabled,
            secret_store: Arc::new(secrets::Unprotected),
            ..EngineOptions::development()
        };
        Engine::start(Profile::new("test", dir).unwrap(), options)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ping_replies_and_announces() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path()).await;
        let mut events = engine.subscribe();
        let reply = engine.handle(Command::Ping { nonce: 7 }).await.unwrap();
        assert_eq!(reply, Response::Pong { nonce: 7 });
        assert_eq!(events.recv().await.unwrap(), Event::Pong { nonce: 7 });
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn info_names_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path()).await;
        let Response::Info(info) = engine.handle(Command::GetInfo).await.unwrap() else {
            panic!("expected info");
        };
        assert_eq!(info.profile, "test");
        assert_eq!(info.protocol, "couch-coapp/0");
        engine.shutdown().await;
    }

    /// The TypeScript bindings assume this JSON shape.
    #[test]
    fn commands_and_responses_use_tagged_json() {
        let command: Command = serde_json::from_str(r#"{"type":"ping","nonce":1}"#).unwrap();
        assert_eq!(command, Command::Ping { nonce: 1 });
        let get_info: Command = serde_json::from_str(r#"{"type":"getInfo"}"#).unwrap();
        assert_eq!(get_info, Command::GetInfo);
        let join: Command =
            serde_json::from_str(r#"{"type":"joinRoom","invite":"x","password":"y"}"#).unwrap();
        assert_eq!(
            join,
            Command::JoinRoom {
                invite: "x".into(),
                password: "y".into()
            }
        );
        let reply = serde_json::to_string(&Response::Pong { nonce: 1 }).unwrap();
        assert_eq!(reply, r#"{"type":"pong","nonce":1}"#);
        let link = serde_json::to_string(&MemberLink::Relay).unwrap();
        assert_eq!(link, r#""relay""#);
    }
}
