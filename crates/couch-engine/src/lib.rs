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

pub mod build_config;
pub mod pipeline;
mod profile;
mod rooms;
pub mod secrets;
mod store;

use std::{
    fmt,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use build_config::BuiltInRelay;
use couch_input::shortcuts::{ShortcutError, Shortcuts};
use couch_net::endpoint::RelayChoice;
use couch_proto::{invite::InviteError, session::SeatKind};
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
    /// Every keyboard shortcut, with the user's keys.
    GetShortcuts,
    /// Moves a shortcut to another key, keeping its modifiers. `code` is the
    /// browser's `KeyboardEvent.code` of the key pressed ("KeyP"). Answers
    /// with every shortcut. The panic key changes at the next Go live, the
    /// viewer's keys in the next stream window.
    SetShortcut {
        id: String,
        code: String,
    },
    /// Puts one shortcut back on its default key, or all of them with
    /// `None`. Answers with every shortcut.
    ResetShortcuts {
        id: Option<String>,
    },
    /// The relay setting, saved and in use, and the relay we are reachable
    /// through now.
    GetNetwork,
    /// Saves the relay setting; it applies when the app starts again.
    SetNetwork {
        relay: RelaySetting,
    },
    /// Whether ViGEmBus and HidHide are installed.
    GetDrivers,
    /// Runs a driver's installer, shipped with the app, with Windows'
    /// administrator prompt; answers when it has finished, with every
    /// driver's state.
    InstallDriver {
        driver: DriverName,
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
        /// How the game takes input: its seats. Keyboard and mouse need a
        /// window source; they go in only while it is in front. Remembered
        /// for the window's program.
        input: InputSetup,
        /// The host's own controller, to play through a seat: hidden from
        /// games while live, it drives the first seat with a pad. `None`
        /// plays outside the seats, with whatever the game sees directly.
        controller: Option<HostController>,
    },
    StopLive,
    /// Watches `host`'s stream in `room`, in a viewer window. Answers once
    /// the host has.
    Watch {
        room: String,
        host: String,
    },
    StopWatching,
    /// While watching: asks the host for a seat (free, or someone else's).
    RequestControl {
        seat: u8,
    },
    /// While watching: withdraws the request, or gives the seat back.
    CancelControl,
    /// While streaming: accepts or declines a viewer's request.
    AnswerControl {
        member: String,
        accept: bool,
    },
    /// While streaming: hands a seat to a viewer who didn't ask. A viewer
    /// holding another seat moves.
    PassControl {
        member: String,
        seat: u8,
    },
    /// While streaming: take a seat back from a viewer, or leave the one our
    /// controller plays, or every seat but ours with `None`. The seat stays
    /// empty; our controller takes one again only with [`Self::PlaySeat`].
    TakeBackControl {
        seat: Option<u8>,
    },
    /// While streaming, with our controller playing through the seats: play
    /// this seat with it. A viewer on that seat loses it.
    PlaySeat {
        seat: u8,
    },
    /// While streaming: let viewers have control again after the panic key.
    AllowControl,
}

/// One row of [`Command::GetShortcuts`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ShortcutView {
    /// Names the row for [`Command::SetShortcut`].
    pub id: String,
    /// Such as "Ctrl+Alt+Shift+H".
    pub keys: String,
    /// Ctrl+Alt+Shift come with the key; otherwise only F-keys are allowed.
    pub chord: bool,
    /// On its default key.
    pub is_default: bool,
    pub action: String,
    pub scope: ShortcutScope,
    /// When it applies, if not always; empty otherwise.
    pub condition: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ShortcutScope {
    /// In the stream window.
    Viewer,
    /// Anywhere on this PC while streaming.
    Host,
}

/// Which relay server the app uses to start connections and to carry them
/// when a direct path fails. Saved per profile; applies at the next start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum RelaySetting {
    /// The relay this build was made with (ours, with its token), or
    /// iroh's public relays in a build without one (development). The
    /// default, so a profile that never chose follows the build.
    #[default]
    Builtin,
    /// iroh's public relays: for testing only (their terms cover hobby use).
    Public,
    /// One relay server by URL, such as your own (`iroh-relay`). No
    /// third-party address lookup, and no token.
    Custom(String),
}

impl RelaySetting {
    /// What the endpoint binds with, in this build.
    pub fn choice(&self) -> Result<RelayChoice, EngineError> {
        self.choice_in(build_config::relay().as_ref(), build_config::test_build())
    }

    /// What the endpoint binds with in a build with `built_in` as its relay.
    /// A custom URL must be https; a test build also takes http on this PC
    /// or a private (LAN) address, and any build takes http on this PC.
    fn choice_in(
        &self,
        built_in: Option<&BuiltInRelay>,
        test_build: bool,
    ) -> Result<RelayChoice, EngineError> {
        match self {
            Self::Builtin => {
                Ok(
                    built_in.map_or(RelayChoice::N0Public, |relay| RelayChoice::Custom {
                        urls: vec![relay.url.clone()],
                        token: relay.token.clone(),
                    }),
                )
            }
            Self::Public => Ok(RelayChoice::N0Public),
            Self::Custom(url) => {
                let invalid = |why: &str| EngineError::InvalidRelay(why.to_string());
                let parsed: couch_net::RelayUrl = url
                    .trim()
                    .parse()
                    .map_err(|_| invalid("that isn't a web address"))?;
                let host = parsed.host_str().unwrap_or_default();
                let local = matches!(host, "localhost" | "127.0.0.1");
                let lan = host
                    .parse::<std::net::Ipv4Addr>()
                    .is_ok_and(|ip| ip.is_private());
                match parsed.scheme() {
                    "https" => {}
                    "http" if local || (lan && test_build) => {}
                    _ => return Err(invalid("it must start with https://")),
                }
                if parsed.host_str().is_none_or(str::is_empty) {
                    return Err(invalid("it names no server"));
                }
                // The setting is logged and shown: no credentials in it.
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(invalid("it can't carry a user name or password"));
                }
                Ok(RelayChoice::custom(vec![parsed]))
            }
        }
    }
}

/// Answer to [`Command::GetNetwork`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NetworkView {
    /// What applies at the next start.
    pub saved: RelaySetting,
    /// The relay this build was made with (its URL), shown for
    /// [`RelaySetting::Builtin`]; `None` in development builds, where that
    /// choice means iroh's public relays.
    pub built_in: Option<String>,
    /// Plain http relays at private (LAN) addresses are allowed: a
    /// development or test build.
    pub test_build: bool,
    /// What this run uses; `None` without relays (tests). A saved setting
    /// this build doesn't take runs as the build's own, so the two can
    /// differ.
    pub in_use: Option<RelaySetting>,
    /// The relay we are reachable through now; `None` until connected.
    pub home_relay: Option<String>,
    /// The saved setting differs from the one in use: it applies at the
    /// next start. Compared as iroh reads URLs (`https://a.b` is in use as
    /// `https://a.b/`), never as typed.
    pub restart_needed: bool,
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
    /// Game controllers plugged into this PC. In a multiplayer game, one
    /// can take a guest's place, so Go live warns.
    pub controllers: u32,
    /// This PC's own controllers that could play through a seat (never our
    /// virtual pads, nor a controller with their ids).
    pub host_controllers: Vec<HostController>,
    /// Whether a controller can be hidden from games (HidHide).
    pub hiding: HidingView,
    /// Whether virtual controllers can be plugged in (ViGEmBus). Without
    /// them, guests' controllers can't reach the game: Go live warns.
    pub virtual_pads: bool,
}

/// One of the host's own controllers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HostController {
    pub name: String,
    /// Its USB ids, which find it again after it is reconnected.
    pub vendor: u16,
    pub product: u16,
}

/// Whether the host's controller can be hidden from games.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum HidingView {
    /// HidHide isn't installed: the controller can't play through a seat.
    NotInstalled,
    Ready,
    /// Something held HidHide for over a second, most likely its
    /// configuration window.
    Busy,
}

/// How the game takes input, chosen when going live and remembered per
/// program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "mode", rename_all = "camelCase")]
#[ts(export)]
pub enum InputSetup {
    /// One player at a time, on one seat.
    Single { seat: SeatChoice },
    /// A seat per player: 1 to 4 controllers, each its own virtual pad, and
    /// optionally one seat with keyboard and mouse.
    #[serde(rename_all = "camelCase")]
    Multi {
        controllers: u8,
        keyboard_mouse: bool,
    },
}

/// What a single-player seat takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SeatChoice {
    Controller,
    KeyboardMouse,
    Both,
}

impl Default for InputSetup {
    /// What guests get unless the host says otherwise: a controller.
    fn default() -> Self {
        Self::Single {
            seat: SeatChoice::Controller,
        }
    }
}

impl InputSetup {
    /// The seats, by seat id: the controllers first, then the keyboard seat.
    /// Not checked: `couch_room::session` refuses a bad setup.
    pub fn seats(&self) -> Vec<SeatKind> {
        match *self {
            Self::Single { seat } => vec![match seat {
                SeatChoice::Controller => SeatKind::Controller,
                SeatChoice::KeyboardMouse => SeatKind::KeyboardMouse,
                SeatChoice::Both => SeatKind::Both,
            }],
            Self::Multi {
                controllers,
                keyboard_mouse,
            } => {
                let mut seats = vec![SeatKind::Controller; usize::from(controllers)];
                if keyboard_mouse {
                    seats.push(SeatKind::KeyboardMouse);
                }
                seats
            }
        }
    }

    pub fn keyboard_mouse(&self) -> bool {
        self.seats().iter().any(|s| s.has_keyboard_mouse())
    }
}

/// One seat of a [`SessionView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SeatView {
    /// Such as "Controller 2" or "Keyboard and mouse".
    pub label: String,
    /// It drives a virtual controller.
    pub pad: bool,
    pub keyboard_mouse: bool,
    /// Who plays it: a viewer, or the host with their own controller.
    /// `None` while free.
    pub holder: Option<String>,
    /// The host plays it with their own controller.
    pub host: bool,
}

/// A viewer asking for a seat, for the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RequestView {
    pub member: String,
    pub seat: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WindowView {
    pub id: String,
    pub title: String,
    /// The program's file name; may be empty.
    pub app: String,
    /// The program runs as administrator (and we don't): guests' keyboard
    /// and mouse can't reach it, and the panic key doesn't work while it is
    /// in front (measured in M6).
    pub elevated: bool,
    /// The input setup last used with this program, if any.
    pub input_setup: Option<InputSetup>,
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
    pub max_viewers: u8,
    /// Member ids, in the order they joined.
    pub viewers: Vec<String>,
    /// The seats players can hold, by seat id; one is single player.
    pub seats: Vec<SeatView>,
    /// The host's own controller plays only through the seats (hidden from
    /// the game): a seat nobody holds is empty, and the host may play none.
    pub host_on_seats: bool,
    /// The host took control back with the panic key and hasn't allowed it
    /// again.
    pub control_paused: bool,
    pub role: SessionRole,
    /// For the host: viewers asking for a seat, oldest first.
    pub requests: Vec<RequestView>,
    /// For a viewer: the seat it asked for and waits on.
    pub asked_seat: Option<u8>,
    /// For the host: the panic key's keys, while its hook is in place;
    /// `None` if it couldn't be installed (the app warns).
    pub panic_key: Option<String>,
    /// For the host: the keys that accept and decline the oldest request,
    /// while the hook is in place.
    pub answer_keys: Option<AnswerKeys>,
    /// For the host: where the overlay goes, the work area of the monitor
    /// showing what we stream; `None` where unknown.
    pub overlay_area: Option<ScreenArea>,
    /// For the host: their controller playing through a seat, hidden from
    /// games, if they chose one and it works.
    pub host_controller: Option<String>,
    /// For the host: why the controller they chose doesn't play through a
    /// seat (they play outside the seats instead).
    pub host_controller_problem: Option<String>,
}

/// A driver the app relies on (M10a; Settings → Drivers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum DriverName {
    /// Virtual controllers: guests' controllers reach the host's game.
    Vigembus,
    /// Hides the host's own controller from games, so it can play a seat.
    Hidhide,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DriverState {
    /// Installed and working.
    Ready,
    /// Installed, but not working yet: Windows needs a restart.
    NeedsRestart,
    /// HidHide's configuration window holds it.
    Busy,
    #[default]
    Missing,
}

/// One driver in [`Command::GetDrivers`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DriverView {
    pub driver: DriverName,
    pub state: DriverState,
    /// Its installer ships with this build (Install can run it).
    pub installer: bool,
}

/// The host's keys for answering requests from inside a game (M8), such as
/// "Ctrl+Alt+Shift+Y".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnswerKeys {
    pub accept: String,
    pub decline: String,
}

/// A rectangle on the desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScreenArea {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
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
    /// Watching, but the connection to the host went; the stream window
    /// stays open while it comes back (up to 30 s).
    Reconnecting,
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
    Shortcuts { shortcuts: Vec<ShortcutView> },
    Network(NetworkView),
    Drivers { drivers: Vec<DriverView> },
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
            Self::GetShortcuts => f.write_str("GetShortcuts"),
            Self::SetShortcut { id, code } => f
                .debug_struct("SetShortcut")
                .field("id", id)
                .field("code", code)
                .finish(),
            Self::ResetShortcuts { id } => {
                f.debug_struct("ResetShortcuts").field("id", id).finish()
            }
            Self::GetNetwork => f.write_str("GetNetwork"),
            Self::SetNetwork { relay } => {
                f.debug_struct("SetNetwork").field("relay", relay).finish()
            }
            Self::GetDrivers => f.write_str("GetDrivers"),
            Self::InstallDriver { driver } => f
                .debug_struct("InstallDriver")
                .field("driver", driver)
                .finish(),
            Self::ListSources => f.write_str("ListSources"),
            Self::GoLive {
                room,
                fps,
                max_mbps,
                upload_mbps,
                max_viewers,
                input,
                controller,
                ..
            } => f
                .debug_struct("GoLive")
                .field("room", room)
                .field("fps", fps)
                .field("max_mbps", max_mbps)
                .field("upload_mbps", upload_mbps)
                .field("max_viewers", max_viewers)
                .field("input", input)
                .field("controller", controller)
                .finish_non_exhaustive(),
            Self::StopLive => f.write_str("StopLive"),
            Self::Watch { room, host } => f
                .debug_struct("Watch")
                .field("room", room)
                .field("host", host)
                .finish(),
            Self::StopWatching => f.write_str("StopWatching"),
            Self::RequestControl { seat } => f
                .debug_struct("RequestControl")
                .field("seat", seat)
                .finish(),
            Self::CancelControl => f.write_str("CancelControl"),
            Self::AnswerControl { member, accept } => f
                .debug_struct("AnswerControl")
                .field("member", member)
                .field("accept", accept)
                .finish(),
            Self::PassControl { member, seat } => f
                .debug_struct("PassControl")
                .field("member", member)
                .field("seat", seat)
                .finish(),
            Self::TakeBackControl { seat } => f
                .debug_struct("TakeBackControl")
                .field("seat", seat)
                .finish(),
            Self::PlaySeat { seat } => f.debug_struct("PlaySeat").field("seat", seat).finish(),
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
            Self::Shortcuts { shortcuts } => f
                .debug_struct("Shortcuts")
                .field("shortcuts", &shortcuts.len())
                .finish(),
            Self::Network(network) => f.debug_tuple("Network").field(network).finish(),
            Self::Drivers { drivers } => {
                f.debug_struct("Drivers").field("drivers", drivers).finish()
            }
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
    /// Their Couch Co-App can't talk to ours (another wire revision): one
    /// of us needs to update. `None` once we talk again.
    pub other_version: Option<OtherVersion>,
}

/// Another member's build, when it can't talk to ours (M10a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OtherVersion {
    /// Such as "0.9.0".
    pub app_version: String,
    /// Theirs is the newer one, so we need to update; otherwise they do.
    pub newer: bool,
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
    /// A viewer asks for a seat in our stream.
    ControlRequested {
        room: String,
        member: String,
        /// The seat's label, such as "Controller 2".
        seat: String,
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
    #[error(transparent)]
    Shortcut(#[from] ShortcutError),
    #[error("that relay address won't work: {0}")]
    InvalidRelay(String),
    #[error("the driver wasn't installed: {0}")]
    DriverInstall(String),
    /// The other side's Couch Co-App can't talk to ours (M10a).
    #[error("{}", other_version_text(app_version, *newer))]
    OtherVersion { app_version: String, newer: bool },
    /// Capture, encoding or the viewer window failed; the text says how.
    #[error("{0}")]
    Media(String),
}

fn other_version_text(app_version: &str, newer: bool) -> String {
    let ours = env!("CARGO_PKG_VERSION");
    if newer {
        format!(
            "the room's owner runs Couch Co-App {app_version}, newer than yours ({ours}); install the new version to join"
        )
    } else {
        format!(
            "the room's owner runs Couch Co-App {app_version}, older than yours ({ours}); they need to install the new version first"
        )
    }
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
    /// How long a streamed game may take to show a new window before its
    /// session ends ([`pipeline::source::REPLACE_GRACE`]).
    pub replace_grace: Duration,
    /// How long watching waits for the host's connection to come back.
    pub reconnect_wait: Duration,
    /// Use only the relay, no direct paths: tests proving traffic goes
    /// through it.
    pub relay_only: bool,
}

impl EngineOptions {
    /// The desktop app with iroh's public relays, which are for development
    /// only.
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
            replace_grace: pipeline::source::REPLACE_GRACE,
            reconnect_wait: Duration::from_secs(30),
            relay_only: false,
        }
    }

    /// The desktop app: [`Self::development`] with the profile's saved relay
    /// setting (Settings → Network), by default the relay this build was
    /// made with ([`build_config`]). A saved setting this build doesn't take
    /// (a typo, or a LAN http relay saved by a test build) is logged and the
    /// build's own relay is used; Settings shows both.
    pub fn desktop(profile: &Profile) -> Self {
        let saved = store::network(profile.data_dir()).relay;
        if let Some(url) = build_config::unreadable_relay() {
            tracing::error!(
                "this build's relay URL {url:?} doesn't parse; it has no relay of its own"
            );
        }
        let built_in = build_config::relay();
        let relays = saved
            .choice_in(built_in.as_ref(), build_config::test_build())
            .unwrap_or_else(|e| {
                tracing::warn!("the saved relay setting: {e}; using this build's own");
                fallback_relays(built_in.as_ref())
            });
        Self {
            relays,
            ..Self::development()
        }
    }
}

/// What a running endpoint uses, as a setting. The built-in relay is told
/// from the same URL typed as your own by its token.
fn relay_in_use(relays: &RelayChoice, built_in: Option<&BuiltInRelay>) -> Option<RelaySetting> {
    match relays {
        RelayChoice::N0Public => Some(RelaySetting::Public),
        RelayChoice::Custom { urls, token } => {
            let url = urls.first()?;
            let ours = built_in.is_some_and(|b| b.url == *url && b.token == *token);
            Some(if ours {
                RelaySetting::Builtin
            } else {
                RelaySetting::Custom(url.to_string())
            })
        }
        RelayChoice::Disabled => None,
    }
}

/// What runs when the saved setting doesn't apply in this build: the
/// build's own relay, never iroh's public ones in a build that has one.
fn fallback_relays(built_in: Option<&BuiltInRelay>) -> RelayChoice {
    RelaySetting::Builtin
        .choice_in(built_in, false)
        .expect("the built-in choice always applies")
}

/// Whether `saved` would start differently from what runs, in a build
/// with `built_in` as its relay: both as iroh reads them, so a URL typed
/// without iroh's trailing slash still matches. A saved setting this build
/// doesn't take runs as the build's own ([`EngineOptions::desktop`]), so a
/// restart wouldn't change that.
fn restart_needed(
    saved: &RelaySetting,
    in_use: &Option<RelaySetting>,
    built_in: Option<&BuiltInRelay>,
    test_build: bool,
) -> bool {
    let applies_as = saved
        .choice_in(built_in, test_build)
        .unwrap_or_else(|_| fallback_relays(built_in));
    relay_in_use(&applies_as, built_in) != *in_use
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
    /// The relay setting this run's endpoint was bound with.
    relay_in_use: Option<RelaySetting>,
    /// Held while the shortcut keys are read, changed and written, so two
    /// changes at once can't lose one.
    shortcuts_edit: Mutex<()>,
    /// A driver's setup is running ([`Command::InstallDriver`]).
    installing_driver: AtomicBool,
    /// Held while this engine runs; see [`lock_profile`].
    _lock: std::fs::File,
}

impl Engine {
    /// Loads (or creates) the profile's identity, binds the endpoint and
    /// reconnects every remembered room.
    pub async fn start(profile: Profile, options: EngineOptions) -> Result<Self, EngineError> {
        let lock = lock_profile(&profile, options.profile_wait).await?;
        let backend = options.backend.clone();
        // A host here that was killed may have left a guest's keys held.
        let left = backend.release_left_input();
        if left > 0 {
            tracing::warn!("released {left} keys or buttons a crashed host left held");
        }
        let (events, _) = broadcast::channel(256);
        let panic = Arc::new(Mutex::new(None));
        let relay_in_use = relay_in_use(&options.relays, build_config::relay().as_ref());
        let started = rooms::start(&profile, options, events.clone(), panic.clone()).await?;
        Ok(Self {
            profile,
            member_id: started.member_id.to_hex(),
            events,
            actor: started.inbox,
            opened: Mutex::new(None),
            backend,
            panic,
            relay_in_use,
            shortcuts_edit: Mutex::new(()),
            installing_driver: AtomicBool::new(false),
            _lock: lock,
        })
    }

    /// Every driver's state, read off the async runtime (HidHide's may wait
    /// up to a second for its control device).
    async fn drivers(&self) -> Result<Response, EngineError> {
        let backend = self.backend.clone();
        let drivers = tokio::task::spawn_blocking(move || backend.drivers())
            .await
            .map_err(|_| EngineError::Stopped)?;
        Ok(Response::Drivers {
            drivers: drivers
                .into_iter()
                .map(|(driver, state, installer)| DriverView {
                    driver,
                    state,
                    installer,
                })
                .collect(),
        })
    }

    /// Every shortcut as the app lists it, with the user's keys.
    fn shortcut_views(shortcuts: &Shortcuts) -> Response {
        Response::Shortcuts {
            shortcuts: shortcuts
                .rows()
                .iter()
                .map(|b| ShortcutView {
                    id: b.id.to_string(),
                    keys: b.keys(),
                    chord: b.chord,
                    is_default: shortcuts.is_default(b.id),
                    action: b.action.to_string(),
                    scope: match b.scope {
                        couch_input::shortcuts::Scope::Viewer => ShortcutScope::Viewer,
                        couch_input::shortcuts::Scope::Host => ShortcutScope::Host,
                    },
                    condition: b.condition.to_string(),
                })
                .collect(),
        }
    }

    /// Reads the shortcut keys, changes them with `edit` and saves them.
    fn edit_shortcuts(
        &self,
        edit: impl FnOnce(&mut Shortcuts) -> Result<(), ShortcutError>,
    ) -> Result<Response, EngineError> {
        let _editing = self
            .shortcuts_edit
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let dir = self.profile.data_dir();
        let mut shortcuts = store::shortcuts(dir);
        edit(&mut shortcuts)?;
        store::save_shortcuts(dir, &shortcuts)?;
        Ok(Self::shortcut_views(&shortcuts))
    }

    /// The host's panic key: every seat of our stream back to us at once,
    /// pending requests declined, and control paused until
    /// [`Command::AllowControl`]. Does nothing while not streaming.
    pub async fn panic_key(&self) {
        // Released first, straight through the input task.
        if let Some(handle) = &*self.panic.lock().unwrap_or_else(PoisonError::into_inner) {
            handle.panic();
        }
        let _ = self.actor.send(rooms::Input::Panic(None)).await;
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
            Command::GetShortcuts => Ok(Self::shortcut_views(&store::shortcuts(
                self.profile.data_dir(),
            ))),
            Command::SetShortcut { id, code } => self.edit_shortcuts(|s| {
                let key = couch_input::shortcuts::key_for_code(&code)
                    .ok_or(ShortcutError::KeyNotAllowed)?;
                s.set(&id, key.usage)
            }),
            Command::ResetShortcuts { id } => self.edit_shortcuts(|s| match id {
                Some(id) => s.reset(&id),
                None => {
                    *s = Shortcuts::default();
                    Ok(())
                }
            }),
            Command::GetNetwork => {
                let (reply, home) = oneshot::channel();
                self.actor
                    .send(rooms::Input::HomeRelay(reply))
                    .await
                    .map_err(|_| EngineError::Stopped)?;
                let saved = store::network(self.profile.data_dir()).relay;
                let built_in = build_config::relay();
                let test_build = build_config::test_build();
                Ok(Response::Network(NetworkView {
                    restart_needed: restart_needed(
                        &saved,
                        &self.relay_in_use,
                        built_in.as_ref(),
                        test_build,
                    ),
                    saved,
                    built_in: built_in.map(|relay| relay.url.to_string()),
                    test_build,
                    in_use: self.relay_in_use.clone(),
                    home_relay: home.await.map_err(|_| EngineError::Stopped)?,
                }))
            }
            Command::SetNetwork { relay } => {
                // Checked now, so a typo shows here rather than at the next
                // start.
                relay.choice()?;
                let relay = match relay {
                    RelaySetting::Custom(url) => RelaySetting::Custom(url.trim().to_string()),
                    other => other,
                };
                let dir = self.profile.data_dir();
                store::save_network(dir, &store::NetworkSettings { relay })?;
                Ok(Response::Done)
            }
            Command::ListSources => {
                let backend = self.backend.clone();
                let (sources, controllers, host_controllers, hiding, virtual_pads) =
                    tokio::task::spawn_blocking(move || {
                        // ViGEmBus only: HidHide's state is read below, and
                        // reading it twice could wait twice.
                        let virtual_pads = backend.virtual_pads();
                        let host_controllers = match backend.local_pads().map(|p| p.list()) {
                            Some(Ok(list)) => list,
                            Some(Err(e)) => {
                                tracing::warn!("listing this PC's controllers: {e}");
                                Vec::new()
                            }
                            None => Vec::new(),
                        };
                        (
                            backend.sources(),
                            backend.controllers(),
                            host_controllers,
                            backend.hiding(),
                            virtual_pads,
                        )
                    })
                    .await
                    .map_err(|_| EngineError::Stopped)?;
                let sources = sources.map_err(|e| EngineError::Media(e.to_string()))?;
                let setups = store::input_setups(self.profile.data_dir());
                Ok(Response::Sources(SourceList {
                    windows: sources
                        .windows
                        .into_iter()
                        .map(|w| WindowView {
                            id: w.id.to_string(),
                            input_setup: setups.get(&w.app.to_lowercase()).copied(),
                            title: w.title,
                            app: w.app,
                            elevated: w.elevated,
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
                    controllers: u32::try_from(controllers).unwrap_or(u32::MAX),
                    host_controllers: host_controllers
                        .into_iter()
                        .map(|c| HostController {
                            name: c.name,
                            vendor: c.ids.vendor,
                            product: c.ids.product,
                        })
                        .collect(),
                    hiding: match hiding {
                        pipeline::Hiding::Unavailable => HidingView::NotInstalled,
                        pipeline::Hiding::Ready => HidingView::Ready,
                        pipeline::Hiding::Busy => HidingView::Busy,
                    },
                    virtual_pads,
                }))
            }
            Command::GetDrivers => self.drivers().await,
            Command::InstallDriver { driver } => {
                // One setup at a time: a second, started from a reopened
                // Settings, would fail in the driver's installer.
                if self.installing_driver.swap(true, Ordering::SeqCst) {
                    return Err(EngineError::Refused(
                        "a driver's setup is already running; finish it first",
                    ));
                }
                let backend = self.backend.clone();
                let result =
                    tokio::task::spawn_blocking(move || backend.install_driver(driver)).await;
                self.installing_driver.store(false, Ordering::SeqCst);
                let result = result.map_err(|_| EngineError::Stopped)?;
                if let Err(why) = result {
                    tracing::warn!(?driver, "installing a driver: {why}");
                    return Err(EngineError::DriverInstall(why));
                }
                tracing::info!(?driver, "a driver's installer finished");
                self.drivers().await
            }
            Command::GoLive {
                source: SourceChoice::Window { ref id },
                input,
                ..
            } => {
                let window = id.parse::<pipeline::NativeId>().ok();
                let response = self.forward(command).await;
                // Live: remember the setup for the window's program.
                if response.is_ok()
                    && let Some(window) = window
                {
                    let backend = self.backend.clone();
                    let dir = self.profile.data_dir().to_path_buf();
                    tokio::task::spawn_blocking(move || {
                        let app = backend
                            .sources()
                            .ok()
                            .and_then(|s| s.windows.into_iter().find(|w| w.id == window))
                            .map(|w| w.app)
                            .filter(|app| !app.is_empty());
                        if let Some(app) = app
                            && let Err(e) = store::save_input_setup(&dir, &app, input)
                        {
                            tracing::warn!("couldn't remember the input setup: {e}");
                        }
                    });
                }
                response
            }
            command => self.forward(command).await,
        }
    }

    /// Hands a command to the room actor and waits for its answer.
    async fn forward(&self, command: Command) -> Result<Response, EngineError> {
        let (reply, response) = oneshot::channel();
        self.actor
            .send(rooms::Input::Command(command, reply))
            .await
            .map_err(|_| EngineError::Stopped)?;
        response.await.map_err(|_| EngineError::Stopped)?
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

    /// For tests: drops the connection with `member` in `room` as a dead
    /// path would (closed as timed out); the room redials.
    #[doc(hidden)]
    pub async fn drop_connection(&self, room: &str, member: &str) {
        let _ = self
            .actor
            .send(rooms::Input::DropConnection(
                room.to_string(),
                member.to_string(),
            ))
            .await;
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
                input: InputSetup::default(),
                controller: None,
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
                    elevated: false,
                    input_setup: None,
                }],
                screens: Vec::new(),
                controllers: 0,
                host_controllers: Vec::new(),
                hiding: HidingView::NotInstalled,
                virtual_pads: true,
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

    #[tokio::test]
    async fn shortcuts_list_the_panic_key_first() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path()).await;
        let Response::Shortcuts { shortcuts } = engine.handle(Command::GetShortcuts).await.unwrap()
        else {
            panic!("expected shortcuts");
        };
        assert_eq!(shortcuts[0].keys, "Ctrl+Alt+Shift+Backspace");
        assert_eq!(shortcuts[0].scope, ShortcutScope::Host);
        assert!(shortcuts[0].is_default && shortcuts[0].chord);
        assert_eq!(shortcuts.len(), couch_input::shortcuts::SHORTCUTS.len());
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn shortcuts_move_are_refused_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path()).await;
        let set = |id: &str, code: &str| {
            engine.handle(Command::SetShortcut {
                id: id.into(),
                code: code.into(),
            })
        };
        let Response::Shortcuts { shortcuts } = set("panic", "KeyP").await.unwrap() else {
            panic!("expected shortcuts");
        };
        assert_eq!(shortcuts[0].keys, "Ctrl+Alt+Shift+P");
        assert!(!shortcuts[0].is_default);
        // Kept for the next start.
        let data = engine.profile().data_dir().to_path_buf();
        assert_eq!(
            store::shortcuts(&data)
                .binding(couch_input::shortcuts::Shortcut::Panic)
                .keys(),
            "Ctrl+Alt+Shift+P"
        );

        // A key another row has, one not allowed, a row that doesn't exist.
        let taken = set("fullscreen", "KeyP").await.unwrap_err().to_string();
        assert!(
            taken.contains("Ctrl+Alt+Shift+P already does this"),
            "{taken}"
        );
        assert!(set("panic", "Escape").await.is_err());
        assert!(set("panic", "NotAKey").await.is_err());
        assert!(set("nothing", "KeyQ").await.is_err());
        assert!(set("toggleHudAlone", "KeyQ").await.is_err());

        set("fullscreen", "KeyJ").await.unwrap();
        engine
            .handle(Command::ResetShortcuts {
                id: Some("panic".into()),
            })
            .await
            .unwrap();
        let shortcuts = store::shortcuts(&data);
        assert!(shortcuts.is_default("panic"));
        assert!(!shortcuts.is_default("fullscreen"));
        engine
            .handle(Command::ResetShortcuts { id: None })
            .await
            .unwrap();
        assert_eq!(store::shortcuts(&data), Shortcuts::default());
        engine.shutdown().await;
    }

    /// Reads the drivers' states on this machine; installs nothing.
    #[cfg(windows)]
    #[tokio::test]
    async fn both_drivers_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path()).await;
        let Response::Drivers { drivers } = engine.handle(Command::GetDrivers).await.unwrap()
        else {
            panic!("expected the drivers");
        };
        assert_eq!(
            drivers.iter().map(|d| d.driver).collect::<Vec<_>>(),
            [DriverName::Vigembus, DriverName::Hidhide]
        );
        engine.shutdown().await;
    }

    #[test]
    fn a_relay_url_in_use_matches_it_as_typed() {
        let restart = |saved: &RelaySetting, in_use: &Option<RelaySetting>| {
            restart_needed(saved, in_use, None, false)
        };
        let typed = RelaySetting::Custom("https://relay.example.com".into());
        let running = relay_in_use(&typed.choice_in(None, false).unwrap(), None);
        assert_eq!(
            running,
            Some(RelaySetting::Custom("https://relay.example.com/".into())),
            "iroh's own form"
        );
        assert!(!restart(&typed, &running));
        assert!(restart(&RelaySetting::Public, &running));
        assert!(restart(&typed, &Some(RelaySetting::Public)));
        assert!(!restart(&RelaySetting::Public, &Some(RelaySetting::Public)));
        // Broken: it runs as public, and restarting changes nothing.
        let broken = RelaySetting::Custom("nonsense".into());
        assert!(!restart(&broken, &Some(RelaySetting::Public)));
    }

    fn ours() -> BuiltInRelay {
        BuiltInRelay {
            url: "https://relay.couch.example".parse().unwrap(),
            token: couch_net::endpoint::RelayToken::new("t"),
        }
    }

    #[test]
    fn the_default_follows_the_build() {
        // A development build has no relay: the default is iroh's.
        let dev = RelaySetting::Builtin.choice_in(None, true).unwrap();
        assert!(matches!(dev, RelayChoice::N0Public));
        assert_eq!(relay_in_use(&dev, None), Some(RelaySetting::Public));
        assert!(!restart_needed(
            &RelaySetting::Builtin,
            &Some(RelaySetting::Public),
            None,
            true
        ));

        // A release build: ours, with its token.
        let built_in = ours();
        let release = RelaySetting::Builtin
            .choice_in(Some(&built_in), false)
            .unwrap();
        let RelayChoice::Custom { urls, token } = &release else {
            panic!("expected our relay");
        };
        assert_eq!(urls, std::slice::from_ref(&built_in.url));
        assert_eq!(token, &built_in.token);
        let running = relay_in_use(&release, Some(&built_in));
        assert_eq!(running, Some(RelaySetting::Builtin));
        assert!(!restart_needed(
            &RelaySetting::Builtin,
            &running,
            Some(&built_in),
            false
        ));

        // Choices saved before keep applying.
        assert!(matches!(
            RelaySetting::Public.choice_in(Some(&built_in), false),
            Ok(RelayChoice::N0Public)
        ));
        assert!(restart_needed(
            &RelaySetting::Public,
            &running,
            Some(&built_in),
            false
        ));
        // A LAN relay a test build saved doesn't apply in a release build:
        // it runs on ours, never on iroh's, and restarting changes nothing.
        let lan = RelaySetting::Custom("http://192.168.1.10:3340".into());
        assert!(lan.choice_in(Some(&built_in), false).is_err());
        assert!(matches!(
            fallback_relays(Some(&built_in)),
            RelayChoice::Custom { .. }
        ));
        assert!(!restart_needed(&lan, &running, Some(&built_in), false));

        // Our URL typed as your own carries no token, so it isn't ours.
        let typed = RelaySetting::Custom(built_in.url.to_string());
        let typed = typed.choice_in(Some(&built_in), false).unwrap();
        assert!(matches!(
            relay_in_use(&typed, Some(&built_in)),
            Some(RelaySetting::Custom(_))
        ));
    }

    #[test]
    fn plain_http_relays_on_the_lan_only_in_test_builds() {
        let at = |url: &str, test_build| {
            RelaySetting::Custom(url.into())
                .choice_in(None, test_build)
                .is_ok()
        };
        for lan in [
            "http://192.168.1.10:3340",
            "http://10.0.0.5:3340",
            "http://172.20.1.1:3340",
        ] {
            assert!(at(lan, true), "{lan}");
            assert!(!at(lan, false), "{lan}");
        }
        for anywhere in ["http://localhost:3340", "http://127.0.0.1:3340"] {
            assert!(at(anywhere, false), "{anywhere}");
        }
        for never in [
            "http://8.8.8.8:3340",
            "http://172.32.0.1:3340",
            "http://relay.example.com",
        ] {
            assert!(!at(never, true), "{never}");
        }
    }

    #[tokio::test]
    async fn the_relay_setting_is_checked_saved_and_applies_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path()).await;
        let network = || async {
            let Response::Network(view) = engine.handle(Command::GetNetwork).await.unwrap() else {
                panic!("expected the network view");
            };
            view
        };
        let first = network().await;
        assert_eq!(first.saved, RelaySetting::Builtin);
        assert_eq!(first.built_in, None, "a development build");
        assert!(first.test_build);
        assert_eq!(first.in_use, None, "the tests run without relays");
        assert_eq!(first.home_relay, None);

        for bad in [
            "relay.example.com",
            "http://relay.example.com",
            "ftp://relay.example.com",
            "https://",
            "https://user:token@relay.example.com",
        ] {
            assert!(
                engine
                    .handle(Command::SetNetwork {
                        relay: RelaySetting::Custom(bad.into()),
                    })
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        // The last one is checked at the next start, below.
        for good in [
            " https://relay.example.com ",
            "http://192.168.1.10:3340",
            "http://localhost:3340",
        ] {
            engine
                .handle(Command::SetNetwork {
                    relay: RelaySetting::Custom(good.into()),
                })
                .await
                .unwrap();
            assert_eq!(
                network().await.saved,
                RelaySetting::Custom(good.trim().into())
            );
        }
        engine.shutdown().await;

        // The next start binds with it.
        let profile = Profile::new("test", dir.path()).unwrap();
        let RelayChoice::Custom { urls, .. } = EngineOptions::desktop(&profile).relays else {
            panic!("expected our relay");
        };
        assert_eq!(urls[0].host_str(), Some("localhost"));
        // A hand-edited file that no longer parses falls back to public.
        std::fs::write(
            profile.data_dir().join("network.json"),
            br#"{"relay": {"custom": "nonsense"}}"#,
        )
        .unwrap();
        assert!(matches!(
            EngineOptions::desktop(&profile).relays,
            RelayChoice::N0Public
        ));
    }

    #[test]
    fn invites_go_on_the_invite_page_when_the_build_has_one() {
        use couch_proto::{
            ids::{InviteToken, MemberId, RoomNonce},
            invite::{Invite, LINK_PREFIX},
            room::PeerAddr,
        };
        let invite = Invite {
            owner: MemberId([1; 32]),
            nonce: RoomNonce([2; 16]),
            name: "Couch".into(),
            token: InviteToken([4; 32]),
            expires: 1_800_000_000,
            addr: PeerAddr {
                relay: Some("https://relay.example.com./".into()),
                ips: vec![],
            },
        };
        let plain = rooms::invite_link(&invite, None);
        assert!(plain.starts_with(LINK_PREFIX));
        let page = rooms::invite_link(&invite, Some("https://couch.example/j/"));
        assert_eq!(
            page,
            format!("https://couch.example/j/#{}", invite.payload())
        );
        // The app's Join box and the page read the same link.
        assert_eq!(Invite::parse(&page), Ok(invite));
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
