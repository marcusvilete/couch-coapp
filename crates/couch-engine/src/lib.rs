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

pub mod pipeline;
mod profile;
mod rooms;
pub mod secrets;
mod store;

use std::{
    fmt,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use couch_net::endpoint::RelayChoice;
use couch_proto::invite::InviteError;
use couch_room::{
    limits::LimitConfig,
    room::{RoomError, Timings},
    session::SessionError,
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
    Ping {
        nonce: u32,
    },
    /// This profile's display name and member id.
    GetProfile,
    /// Shown to the other members of every room.
    SetDisplayName {
        name: String,
    },
    /// Every remembered room.
    ListRooms,
    /// A new room owned by this profile. Only the owner lets people in.
    CreateRoom {
        name: String,
        password: String,
    },
    /// What an invite (a link or its bare payload) leads to, shown before
    /// asking for the password. Nothing is sent anywhere.
    PreviewInvite {
        invite: String,
    },
    /// The invite link the app was last opened with, if any. Taken: a
    /// second call answers `None`. Opening a link only fills in the join
    /// screen; joining still needs the password and a click.
    TakeOpenedInvite,
    /// Joins with an invite link (or its bare payload) and the password.
    /// The room's owner must be online.
    JoinRoom {
        invite: String,
        password: String,
    },
    /// Owner only: a new invite link, good for a week (or until revoked, or
    /// five wrong passwords). With relays configured it names our relay,
    /// never our IP address, so right after startup it may wait a few
    /// seconds for the relay.
    GetInvite {
        room: String,
    },
    /// Owner only: newcomers need this password from now on. Members stay,
    /// and invites keep working with the new password.
    ChangePassword {
        room: String,
        password: String,
    },
    /// Owner only: every invite made so far stops working.
    RevokeInvites {
        room: String,
    },
    /// Leaves for good: the others are told, and the room is forgotten. The
    /// owner leaving closes the room for everyone.
    LeaveRoom {
        room: String,
    },
    /// Owner only: takes a member out of the room for good, and revokes
    /// every invite (the banned member may hold one).
    BanMember {
        room: String,
        member: String,
    },
    /// Windows and screens that could be streamed. Nothing is sent anywhere.
    ListSources,
    /// Starts streaming `source` to `room`. One stream at a time, and not
    /// while watching one.
    GoLive {
        room: String,
        source: SourceChoice,
        /// What the room sees it called (1 to 96 bytes).
        title: String,
        fps: u32,
        /// The most each viewer's stream may use, Mbit/s.
        max_mbps: u32,
        /// This PC's upload cap, split between viewers, Mbit/s.
        upload_mbps: Option<u32>,
        /// 1 to 7.
        max_viewers: u8,
        /// Whoever plays may use keyboard and mouse too, while the window is
        /// in front. A window source only.
        keyboard_mouse: bool,
    },
    StopLive,
    /// Watches `host`'s stream in `room`, in a viewer window. Answers once
    /// the host has.
    Watch {
        room: String,
        host: String,
    },
    StopWatching,
    /// While watching: asks the host for control.
    RequestControl,
    /// While watching: withdraws the request, or gives control back.
    CancelControl,
    /// While streaming: accepts or declines a viewer's request.
    AnswerControl {
        member: String,
        accept: bool,
    },
    /// While streaming: hands control to a viewer who didn't ask.
    PassControl {
        member: String,
    },
    /// While streaming: play again.
    TakeBackControl,
    /// While streaming: let viewers have control again after the panic key.
    AllowControl,
}

/// A window or screen from [`Command::ListSources`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum SourceChoice {
    Window { id: String },
    Screen { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SourceList {
    pub windows: Vec<WindowView>,
    pub screens: Vec<ScreenView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WindowView {
    pub id: String,
    pub title: String,
    /// The program's file name; may be empty.
    pub app: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScreenView {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// A live session in a room, as this profile sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionView {
    /// The host's member id.
    pub host: String,
    pub title: String,
    pub keyboard_mouse: bool,
    pub max_viewers: u8,
    /// Member ids, in the order they joined.
    pub viewers: Vec<String>,
    /// The viewer who plays; `None` while the host does.
    pub controller: Option<String>,
    /// The host took control back with the panic key and hasn't allowed it
    /// again.
    pub control_paused: bool,
    pub role: SessionRole,
    /// For the host: viewers asking for control, oldest first.
    pub requests: Vec<String>,
}

/// Where this profile stands in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SessionRole {
    Host,
    /// Asked to watch; the host hasn't answered yet.
    Joining,
    Watching,
    /// Watching, and asked for control.
    Asking,
    Controlling,
    /// Not watching.
    None,
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SessionEnd {
    HostStopped,
    GameExited,
    /// The window or screen being streamed went away.
    SourceLost,
    HostLeftRoom,
    HostShutdown,
    Error,
}

/// Why watching ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum WatchEnd {
    /// The host ended the session.
    Ended,
    /// We stopped, or closed the viewer window.
    Left,
    /// The connection to the host went.
    ConnectionLost,
    /// The host can't stream to us (no encoder free, say).
    Unavailable,
    /// The viewer window couldn't start.
    ViewerFailed,
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
    InvitePreview(InvitePreview),
    OpenedInvite { link: Option<String> },
    Sources(SourceList),
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
            Self::PreviewInvite { .. } => f.write_str("PreviewInvite { .. }"),
            Self::TakeOpenedInvite => f.write_str("TakeOpenedInvite"),
            // The invite holds the token that, with the password, lets one in.
            Self::JoinRoom { .. } => f.write_str("JoinRoom { .. }"),
            Self::GetInvite { room } => f.debug_struct("GetInvite").field("room", room).finish(),
            Self::ChangePassword { room, .. } => f
                .debug_struct("ChangePassword")
                .field("room", room)
                .finish_non_exhaustive(),
            Self::RevokeInvites { room } => {
                f.debug_struct("RevokeInvites").field("room", room).finish()
            }
            Self::LeaveRoom { room } => f.debug_struct("LeaveRoom").field("room", room).finish(),
            Self::BanMember { room, member } => f
                .debug_struct("BanMember")
                .field("room", room)
                .field("member", member)
                .finish(),
            // Window titles are the user's business; the log needn't see them.
            Self::ListSources => f.write_str("ListSources"),
            Self::GoLive {
                room,
                fps,
                max_mbps,
                upload_mbps,
                max_viewers,
                keyboard_mouse,
                ..
            } => f
                .debug_struct("GoLive")
                .field("room", room)
                .field("fps", fps)
                .field("max_mbps", max_mbps)
                .field("upload_mbps", upload_mbps)
                .field("max_viewers", max_viewers)
                .field("keyboard_mouse", keyboard_mouse)
                .finish_non_exhaustive(),
            Self::StopLive => f.write_str("StopLive"),
            Self::Watch { room, host } => f
                .debug_struct("Watch")
                .field("room", room)
                .field("host", host)
                .finish(),
            Self::StopWatching => f.write_str("StopWatching"),
            Self::RequestControl => f.write_str("RequestControl"),
            Self::CancelControl => f.write_str("CancelControl"),
            Self::AnswerControl { member, accept } => f
                .debug_struct("AnswerControl")
                .field("member", member)
                .field("accept", accept)
                .finish(),
            Self::PassControl { member } => f
                .debug_struct("PassControl")
                .field("member", member)
                .finish(),
            Self::TakeBackControl => f.write_str("TakeBackControl"),
            Self::AllowControl => f.write_str("AllowControl"),
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
            Self::InvitePreview(preview) => f.debug_tuple("InvitePreview").field(preview).finish(),
            Self::OpenedInvite { link } => f
                .debug_struct("OpenedInvite")
                .field("link", &link.as_ref().map(|_| ".."))
                .finish(),
            Self::Sources(list) => f
                .debug_struct("Sources")
                .field("windows", &list.windows.len())
                .field("screens", &list.screens.len())
                .finish(),
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
    /// Live sessions: ours, and those of members connected to us.
    pub sessions: Vec<SessionView>,
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

/// What an invite leads to. Anyone can make up an invite, so its name is
/// only what the invite claims until the owner admits us.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InvitePreview {
    /// Lowercase hex.
    pub room: String,
    pub name: String,
    pub status: InviteStatus,
    /// Until the owner stops accepting it; 0 once expired.
    pub expires_in_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum InviteStatus {
    /// Joining can be tried: it needs the password and the owner online.
    Ready,
    Expired,
    /// We are in this room already.
    Member,
    /// Our own room's invite.
    Own,
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
    /// The app was opened with an invite link: [`Command::TakeOpenedInvite`]
    /// has it.
    InviteOpened,
    /// A viewer asks for control of our stream.
    ControlRequested {
        room: String,
        member: String,
    },
    /// Our stream ended other than by [`Command::StopLive`].
    SessionEnded {
        room: String,
        reason: SessionEnd,
    },
    /// Watching ended other than by [`Command::StopWatching`].
    WatchEnded {
        room: String,
        host: String,
        reason: WatchEnd,
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
    /// Another Couch Co-App is running as this profile (or still closing).
    #[error("profile {0:?} is already running in another Couch Co-App")]
    ProfileInUse(String),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("you aren't streaming")]
    NotLive,
    #[error("you aren't watching a stream")]
    NotWatching,
    /// Capture, encoding or the viewer window failed; the text says how.
    #[error("{0}")]
    Media(String),
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
    /// How long to wait for another engine on this profile to finish
    /// closing, as when the app is reopened right after closing it.
    pub profile_wait: Duration,
    /// A fixed UDP port instead of a random one. Members find each other
    /// again after restarts through the relay; without one (a LAN, the
    /// tests) only a fixed port keeps saved addresses valid.
    pub port: Option<u16>,
    /// Capture, encoders, injection and the viewer window: this machine's,
    /// or fakes in tests.
    pub backend: Arc<dyn pipeline::Backend>,
    /// How long a request to watch waits for the host's answer.
    pub join_wait: Duration,
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
            profile_wait: Duration::from_secs(5),
            backend: pipeline::platform_backend(),
            join_wait: Duration::from_secs(10),
        }
    }
}

/// Takes the profile's lock file, waiting up to `wait` for another engine
/// to let go. Two engines on one identity would bring it online twice and
/// write the same room files. The desktop app's single-instance check
/// usually stops a second one earlier, but it lets go before our shutdown
/// finishes, and its lock name is not the folder. The OS releases the lock
/// when the file closes, even after a crash.
async fn lock_profile(profile: &Profile, wait: Duration) -> Result<std::fs::File, EngineError> {
    let io = |source| StoreError::Io {
        path: profile.data_dir().to_path_buf(),
        source,
    };
    std::fs::create_dir_all(profile.data_dir()).map_err(io)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(profile.data_dir().join("engine.lock"))
        .map_err(io)?;
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(EngineError::ProfileInUse(profile.name().to_string()));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(io(e).into()),
        }
    }
}

pub struct Engine {
    profile: Profile,
    member_id: String,
    events: broadcast::Sender<Event>,
    actor: mpsc::Sender<rooms::Input>,
    /// The invite link the app was last opened with, until the UI takes it.
    opened: Mutex<Option<String>>,
    backend: Arc<dyn pipeline::Backend>,
    /// While we stream: releases every seat without waiting for the actor.
    panic: Arc<Mutex<Option<pipeline::host::PanicHandle>>>,
    /// Held while this engine runs; see [`lock_profile`].
    _lock: std::fs::File,
}

impl Engine {
    /// Loads (or creates) the profile's identity, binds the endpoint and
    /// reconnects every remembered room.
    pub async fn start(profile: Profile, options: EngineOptions) -> Result<Self, EngineError> {
        let lock = lock_profile(&profile, options.profile_wait).await?;
        let (events, _) = broadcast::channel(256);
        let backend = options.backend.clone();
        let panic = Arc::new(Mutex::new(None));
        let started = rooms::start(&profile, options, events.clone(), panic.clone()).await?;
        Ok(Self {
            profile,
            member_id: started.member_id.to_hex(),
            events,
            actor: started.inbox,
            opened: Mutex::new(None),
            backend,
            panic,
            _lock: lock,
        })
    }

    /// The host's panic key: every seat of our stream back to us at once,
    /// pending requests declined, and control paused until
    /// [`Command::AllowControl`]. Does nothing while not streaming.
    pub async fn panic_key(&self) {
        // Released first, straight through the input task.
        if let Some(handle) = &*self.panic.lock().unwrap_or_else(PoisonError::into_inner) {
            handle.panic();
        }
        let _ = self.actor.send(rooms::Input::Panic).await;
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Our member id, lowercase hex.
    pub fn member_id(&self) -> &str {
        &self.member_id
    }

    /// The app was opened with `link` (from the OS: a click on a
    /// `couchco://` link, perhaps on a web page). It is kept for the UI,
    /// which asks for the password before anything is sent; a newer link
    /// replaces it. Text too long to be an invite is dropped.
    pub fn open_link(&self, link: &str) {
        if link.len() > couch_proto::invite::MAX_CHARS {
            tracing::warn!(
                len = link.len(),
                "ignored an opened link too long to be an invite"
            );
            return;
        }
        *self.opened.lock().unwrap_or_else(PoisonError::into_inner) = Some(link.to_string());
        let _ = self.events.send(Event::InviteOpened);
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
            Command::TakeOpenedInvite => Ok(Response::OpenedInvite {
                link: self
                    .opened
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take(),
            }),
            Command::ListSources => {
                let backend = self.backend.clone();
                let sources = tokio::task::spawn_blocking(move || backend.sources())
                    .await
                    .map_err(|_| EngineError::Stopped)?
                    .map_err(|e| EngineError::Media(e.to_string()))?;
                Ok(Response::Sources(SourceList {
                    windows: sources
                        .windows
                        .into_iter()
                        .map(|w| WindowView {
                            id: w.id.to_string(),
                            title: w.title,
                            app: w.app,
                        })
                        .collect(),
                    screens: sources
                        .screens
                        .into_iter()
                        .map(|s| ScreenView {
                            id: s.id.to_string(),
                            name: s.name,
                            width: s.width,
                            height: s.height,
                            primary: s.primary,
                        })
                        .collect(),
                }))
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

    #[test]
    fn window_titles_stay_out_of_debug() {
        let shown = format!(
            "{:?}",
            Command::GoLive {
                room: "00".into(),
                source: SourceChoice::Window { id: "42".into() },
                title: "Bank of Somewhere - Chrome".into(),
                fps: 60,
                max_mbps: 20,
                upload_mbps: None,
                max_viewers: 4,
                keyboard_mouse: false,
            }
        );
        assert!(!shown.contains("Bank"), "{shown}");
        let list = format!(
            "{:?}",
            Response::Sources(SourceList {
                windows: vec![WindowView {
                    id: "1".into(),
                    title: "Bank of Somewhere - Chrome".into(),
                    app: "chrome.exe".into(),
                }],
                screens: Vec::new(),
            })
        );
        assert!(!list.contains("Bank"), "{list}");
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

    /// Two engines on one profile would bring the same identity online twice
    /// and write the same room files.
    #[tokio::test]
    async fn one_engine_per_profile() {
        let dir = tempfile::tempdir().unwrap();
        let first = engine(dir.path()).await;
        let options = EngineOptions {
            relays: RelayChoice::Disabled,
            secret_store: Arc::new(secrets::Unprotected),
            profile_wait: Duration::from_millis(300),
            ..EngineOptions::development()
        };
        let again =
            |name: &str| Engine::start(Profile::new(name, dir.path()).unwrap(), options.clone());
        assert!(matches!(
            again("TEST").await,
            Err(EngineError::ProfileInUse(name)) if name == "test"
        ));
        first.shutdown().await;
        drop(first);
        again("test").await.unwrap().shutdown().await;
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

    #[test]
    fn passwords_and_invites_stay_out_of_debug() {
        let secret = "hunter2-SECRETLINK";
        let shown = [
            format!(
                "{:?}",
                Command::CreateRoom {
                    name: "Couch".into(),
                    password: secret.into()
                }
            ),
            format!(
                "{:?}",
                Command::JoinRoom {
                    invite: secret.into(),
                    password: secret.into()
                }
            ),
            format!(
                "{:?}",
                Command::ChangePassword {
                    room: "00".into(),
                    password: secret.into()
                }
            ),
            format!(
                "{:?}",
                Command::PreviewInvite {
                    invite: secret.into()
                }
            ),
            format!(
                "{:?}",
                Response::Invite {
                    link: secret.into()
                }
            ),
            format!(
                "{:?}",
                Response::OpenedInvite {
                    link: Some(secret.into())
                }
            ),
        ];
        for text in shown {
            assert!(!text.contains(secret), "{text}");
        }
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
