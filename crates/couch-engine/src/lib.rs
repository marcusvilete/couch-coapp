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

pub mod backup;
pub mod board;
pub mod build_config;
mod diagnostics;
mod network;
pub mod pipeline;
mod profile;
pub mod relays;
pub mod remote;
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

use couch_input::{
    local::ControllerKind,
    shortcuts::{ShortcutError, Shortcuts},
};
use couch_net::endpoint::RelayChoice;
use couch_proto::{invite::InviteError, session::SeatKind};
use couch_room::{
    limits::LimitConfig,
    room::{RoomError, Timings},
    session::SessionError,
};
pub use network::{InternetView, NetworkTimings, NetworkTrouble, RelayStateView};
pub use relays::RelayRules;
use relays::SavedRelays;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use ts_rs::TS;

pub use profile::{Profile, ProfileError};
pub use secrets::SecretStore;
pub use store::StoreError;

/// For a development build: removes this user's registration of the link
/// `scheme` if it opens this very program, and never another's, nor the
/// one an installer made for everyone (P5; `couch_platform_win::links`).
/// Returns whether it removed one.
pub fn forget_own_link_registration(scheme: &str) -> std::io::Result<bool> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()?;
        couch_platform_win::links::forget_user_scheme(scheme, &exe)
    }
    #[cfg(not(windows))]
    {
        let _ = scheme;
        Ok(false)
    }
}

/// Whether the webview should draw without WebKitGTK's DMA-BUF renderer:
/// on a virtual machine's GPU, where it left the window blank
/// (`couch_platform_linux::gpu`). Never elsewhere, nor on Windows.
pub fn webview_needs_plain_renderer() -> bool {
    #[cfg(target_os = "linux")]
    {
        couch_platform_linux::gpu::is_virtual()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

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
    /// The relays this profile holds, and the one we are reachable through
    /// now.
    GetNetwork,
    /// Registers a relay (M26a), or gives one already held a new token:
    /// first checked (does it let us in with that token?), then saved and in
    /// use at once. Answers with [`Response::Network`], or
    /// [`Response::RelayCheck`] when the check failed and nothing changed.
    AddRelay {
        url: String,
        token: Option<String>,
        /// From a personal code: never passed on (M26b).
        personal: bool,
        /// Saved without the check: "save anyway", for a relay that is down
        /// for now.
        unchecked: bool,
    },
    /// "Check again" (M26a): every relay's code checked anew, and a new
    /// look at the network. Answers with [`Response::Network`] at once; the
    /// results follow as [`Event::Network`].
    CheckNetwork,
    /// What a pasted relay code holds, to show before adding it.
    ReadRelayCode {
        code: String,
    },
    /// Adds the relay a code names, with its token, as
    /// [`Command::AddRelay`] does.
    AddRelayCode {
        code: String,
        unchecked: bool,
    },
    /// Copies a relay's code to the clipboard, as a secret (out of the
    /// clipboard's history and sync), for the user to hand to friends. Not
    /// for a relay from a personal code.
    ShareRelay {
        url: String,
    },
    /// Takes a relay away, at once; the build's own can't be. Answers with
    /// [`Response::Network`].
    RemoveRelay {
        url: String,
    },
    /// Keeps iroh's public relays on beside ours (they are on anyway while
    /// nothing is registered). Answers with [`Response::Network`].
    SetPublicRelays {
        on: bool,
    },
    /// Lets iroh's public relays stand in while none of ours works (off by
    /// default). Answers with [`Response::Network`].
    SetPublicFallback {
        on: bool,
    },
    /// Who reaches us through a relay: the rooms and members whose record
    /// names it, or whose connection with us runs through it now; and
    /// whether remote play is on. Answers with [`Response::RelayUse`].
    RelayUse {
        url: String,
    },
    /// The wizard asking for a relay is put off: "Remind me later" (in 7
    /// days, or at the next version), or with `never`, "Stay on the public
    /// relays". Answers with [`Response::Network`].
    DeferRelay {
        never: bool,
    },
    /// What to copy when asking someone for help with the connection:
    /// versions, relays by address with their states, the network, and each
    /// room's members' links, numbered. No names, IP addresses, tokens,
    /// passwords or invites. Answers with [`Response::Diagnostics`].
    GetDiagnostics,
    /// Opens the folder holding this profile's logs in the system's file
    /// manager. The webview never names the path.
    OpenLogFolder,
    /// Whether ViGEmBus and HidHide are installed.
    GetDrivers,
    /// Whether this PC shares again, without asking, what was last chosen
    /// in the desktop's screen-sharing dialog (Linux, M16c), after
    /// forgetting that choice if `forget`: the next stream asks again (the
    /// desktops offer no way to revoke it for an app outside Flatpak).
    /// Answers [`Response::SharedChoice`]; elsewhere nothing is remembered.
    SharedChoice {
        forget: bool,
    },
    /// Plays the request chime from this process, where the webview can't
    /// (Linux's AppImage: `couch_platform_linux::chime`); out of the stream,
    /// as this app's own sound. Elsewhere it does nothing: the webview
    /// plays it.
    PlayChime,
    /// Whether a game in exclusive full screen is in front (Windows), for
    /// the host overlay, which must not come up over one: the game would
    /// leave full screen, often minimizing. Answers
    /// [`Response::FullScreenGame`]; elsewhere always no.
    FullScreenGame,
    /// The third-party licences installed next to the app
    /// (`THIRD-PARTY-NOTICES.txt`, `cargo xtask notices`), for Settings → About.
    GetNotices,
    /// Writes a backup of this profile, sealed with `passphrase` (M13,
    /// `backup`): the identity, the rooms (an owned room without its
    /// password key or invites) and the settings. The app asks where
    /// ([`BackupFiles`]); answers [`Response::Backup`].
    ExportProfile {
        passphrase: String,
    },
    /// Restores a backup into this profile, which must have no rooms; the
    /// app asks which file ([`BackupFiles`]), then starts again
    /// (`restart_app`) to use it. Answers [`Response::Backup`].
    RestoreProfile {
        passphrase: String,
    },
    /// Owner only, for a room restored from a backup (M13): stop waiting
    /// for the members still away and sign again. A change made after the
    /// backup that only they hold, a ban among them, may be lost.
    StopCatchingUp {
        room: String,
    },
    /// Owner only (M13): offers the room to `member`, who must be online. It
    /// changes hands once they accept ([`Command::AnswerOffer`]): from then
    /// they let people in, ban and close the room, with a new password of
    /// their own, and our invites stop working.
    OfferRoom {
        room: String,
        member: String,
    },
    /// Owner only: the offer is withdrawn (until they accepted).
    WithdrawOffer {
        room: String,
    },
    /// The owner offered us the room: accept, with the new password
    /// newcomers will need, or decline. Accepting is for good: until the
    /// owner hands the room over or withdraws, it can't be declined, nor
    /// the room left.
    AnswerOffer {
        room: String,
        accept: bool,
        password: Option<String>,
    },
    /// Runs a driver's installer, shipped with the app, with Windows'
    /// administrator prompt; answers when it has finished, with every
    /// driver's state.
    InstallDriver {
        driver: DriverName,
    },
    /// Windows and screens that could be streamed. Nothing is sent anywhere.
    ListSources,
    /// Starts a session in `room`, with nothing streamed yet (M19): the
    /// seats' virtual pads are plugged in and the host's controller hidden,
    /// so a game started afterwards finds them. One session at a time, and
    /// not while watching one.
    StartSession {
        room: String,
        /// What the room sees it called (1 to 96 bytes).
        title: String,
        /// 1 to 7.
        max_viewers: u8,
        /// How the game takes input: its seats, fixed for the session.
        /// Keyboard and mouse need a window as the stream's source; they go
        /// in only while it is in front.
        input: InputSetup,
        /// The host's own controllers, to play through seats (M23): hidden
        /// from games for the session, each drives its own seat with a pad,
        /// in seat order. Empty plays outside the seats, with whatever the
        /// game sees directly. Two of one model are told apart by the order
        /// they are found.
        controllers: Vec<HostController>,
    },
    /// In our session: streams `source` (M19), stopping a stream already
    /// running first.
    StartStream {
        source: SourceChoice,
        /// What the host's panel calls it (a window's title): shown only
        /// here, and kept out of logs.
        name: String,
        fps: u32,
        /// The most each viewer's stream may use, Mbit/s.
        max_mbps: u32,
        /// This PC's upload cap, split between viewers, Mbit/s.
        upload_mbps: Option<u32>,
        /// One program's sound alone, by its name from
        /// [`Command::ListSoundSources`], where the host picks the sound
        /// (Linux, M16c). `None`: a window's own sound on Windows, and
        /// everything but this app for a screen, or on Linux.
        #[serde(default)]
        program: Option<String>,
    },
    /// What plays sound on this PC now, for [`Command::StartStream`]'s
    /// `program`; refused where the sound follows the window (Windows).
    ListSoundSources,
    /// The host's notices that should be up as desktop notifications, where
    /// they stand in for the overlay (Linux, M16c): each new one shown, each
    /// changed one replaced, the rest closed. Refused elsewhere.
    ShowNotices {
        notices: Vec<NoticeView>,
    },
    /// In our session: stops streaming; viewers stay, and keep their seats.
    StopStream,
    /// The windows our stream's game could be, where the host names it on
    /// a shared screen (Linux, M16c-2: the screen-sharing portal never
    /// says), each marked if it has the shared picture's size. Answers
    /// [`Response::GameWindows`]; empty until the desktop's helper has
    /// spoken. Refused elsewhere, and with nothing streamed.
    GameWindows,
    /// Names our stream's game window (an id from [`Command::GameWindows`]),
    /// or none: guests' keyboard and mouse reach it only while it is in
    /// front, and nothing while none is named. Lasts for the stream, or
    /// until the window closes. Only from this PC.
    SetGameWindow {
        id: Option<u32>,
    },
    /// How loud guests hear our stream's sound (P7): 0 to 100% of the game's
    /// turns it down, up to 400% boosts it (peaks rounded off within full
    /// scale). Applies at once, and is remembered for our next sessions.
    SetStreamLevel {
        percent: u16,
    },
    /// Ends our session: input released, the pads unplugged, the host's
    /// controller shown again.
    EndSession,
    /// Watches `host`'s stream in `room`, in a viewer window. Answers once
    /// the host has.
    Watch {
        room: String,
        host: String,
    },
    StopWatching,
    /// While watching: asks the host for a seat (free, or someone else's).
    /// With `controller`, one of ours by slot
    /// ([`GuestControllerView::slot`]), that one plays it, leaving its seat
    /// if we have no controller to spare (P13); the app asks which when we
    /// have two or more.
    RequestControl {
        seat: u8,
        #[serde(default)]
        #[ts(optional)]
        controller: Option<u8>,
    },
    /// While watching: withdraws the request, or gives every seat back.
    CancelControl,
    /// While watching: gives back one of our seats, keeping the others (M18).
    GiveBackSeat {
        seat: u8,
    },
    /// While streaming: accepts or declines a viewer's request.
    AnswerControl {
        member: String,
        accept: bool,
    },
    /// While streaming: hands a seat to a viewer who didn't ask. It is added
    /// to the viewer's seats if they have a controller to spare for it (or it
    /// has no pad); otherwise the viewer moves (M18). With `controller`, one
    /// of the viewer's controllers by its slot
    /// ([`GuestControllerView::slot`]), that controller drives it (P12),
    /// while it is connected; to a seat they hold already, only that
    /// changes.
    PassControl {
        member: String,
        seat: u8,
        #[serde(default)]
        #[ts(optional)]
        controller: Option<u8>,
    },
    /// While streaming: take a seat back from a viewer, or leave the one our
    /// controller plays, or every seat but ours with `None`. The seat stays
    /// empty; our controller takes one again only with [`Self::PlaySeat`].
    TakeBackControl {
        seat: Option<u8>,
    },
    /// With our controllers playing through the seats: play this seat with
    /// our controller `controller` (M23, by its place in the session's
    /// list). A viewer on that seat loses it; on a seat another of ours
    /// plays, the two swap.
    PlaySeat {
        seat: u8,
        controller: u8,
    },
    /// While streaming: let viewers have control again after the panic key.
    AllowControl,
    /// While hosting: stop waiting for the virtual pads Windows is still
    /// setting up (P10). They are unplugged, and their seats go on without
    /// one.
    StopWaitingForPads,
    /// The host: the window we stream to the front (M25b), restored if
    /// minimized. Only ever that window.
    BringToFront,
    /// While hosting: lets those watching draw on the stream, or stops it
    /// (M27a). Off clears the board. A session starts as Settings says
    /// ([`Command::SetDrawingDefault`]).
    SetDrawing {
        on: bool,
    },
    /// While hosting: everything drawn on the stream gone (M27a).
    ClearBoard,
    /// While hosting: stops a member drawing, clearing what they drew, or
    /// lets them draw again (M27a).
    StopDrawing {
        member: String,
        stopped: bool,
    },
    /// While hosting: the host's own operations from its board overlay over
    /// the game (M27b), made in the board's `generation` as the overlay last
    /// heard it. The same checks as a viewer's; a refusal comes back as
    /// [`Event::HostBoard`]. Never from afar.
    HostDraw {
        generation: u32,
        ops: Vec<board::BoardOpView>,
    },
    /// While hosting: the board whole, for the board overlay that just
    /// loaded (M27b), as [`Event::HostBoard`] snapshots.
    HostBoard,
    /// While hosting: lets those watching put pictures on the board, or
    /// stops it (M27e). Off takes every picture off. A session starts as
    /// Settings says ([`Command::SetDrawingDefault`]).
    SetPictures {
        on: bool,
    },
    /// While hosting: the host's own picture from its board overlay (M27e),
    /// made in the board's `generation`: `element` a picture naming `data`,
    /// a PNG or JPEG in base64 the page already made what the board takes.
    /// Checked as a viewer's is; a refusal comes back as
    /// [`Event::HostBoard`]. Never from afar.
    HostPicture {
        generation: u32,
        element: board::BoardElement,
        data: String,
    },
    /// While hosting: drawing over the game ends (M27b; the overlay's Esc or
    /// Done), and the game comes back to the front. Only the host's own key
    /// begins it.
    EndHostDrawing,
    /// While hosting: show what's drawn over the game, or not, for this
    /// session (M27b). Drawing over the game shows it anyway.
    ShowBoardOverlay {
        on: bool,
    },
    /// While hosting: opens the host's own stream window, or closes it
    /// (M27d): the stream as those watching see it, encoded here for it
    /// alone, where the host sees and draws on the board (a game in
    /// exclusive full screen, a Linux host). Opening one that is open brings
    /// it to the front. Never from afar.
    OwnStreamWindow {
        open: bool,
    },
    /// Settings: whether sessions start with drawing on (M27a).
    GetDrawingDefault,
    /// Saves it, for the next session; answers like
    /// [`Command::GetDrawingDefault`].
    SetDrawingDefault {
        on: bool,
        /// And whether they start with pictures allowed (M27e).
        pictures: bool,
    },
    /// Settings → Streaming (M29): how a host captures what it streams.
    GetStreaming,
    /// Saves it, from the next stream on; answers like
    /// [`Command::GetStreaming`].
    SetStreaming {
        /// A monitor in HDR captured in FP16 and converted to SDR (Windows).
        hdr: bool,
    },
    /// While watching: the stream window's volume (P11), 0 to 100%, saved
    /// for the profile as the window's own slider is.
    SetStreamVolume {
        percent: u8,
        muted: bool,
    },
    /// While watching: something for the stream window itself (P11).
    StreamWindow {
        request: StreamWindowRequest,
    },
    /// Settings → Stream window (P11): how stream windows open.
    GetStreamWindow,
    /// Saves it; a stream window open now follows at once.
    SetStreamWindow {
        bar: bool,
    },
    /// Whether the app keeps an icon in the notification area (M25c): with
    /// it, closing the window hides it there, and starting with Windows
    /// stays there.
    GetTray,
    /// Saves it; the app's icon follows at once.
    SetTray {
        on: bool,
    },
    // ---- Remote play (M25, ADR 0010) ----
    /// Remote play's settings: on the home PC, the devices it trusts and the
    /// members that could be added; on a device, the home PCs it accepted.
    GetRemotePlay,
    /// At the home PC: turns remote play on (a PIN set first) or off. Off
    /// locks every device out at once.
    SetRemotePlay {
        on: bool,
    },
    /// This app starts when the user signs in (M25c: on Windows to the
    /// notification area; on Linux an AppImage, M16c), or stops doing so.
    /// Only where [`RemotePlayView::start_at_sign_in`] offers it.
    SetStartAtSignIn {
        on: bool,
    },
    /// At the home PC: the PIN its devices unlock with. A new one locks them
    /// out until they give it.
    SetRemotePin {
        pin: String,
    },
    /// At the home PC: trusts `member`, a member of one of our rooms, as one
    /// of our own devices. Never possible from afar.
    AddRemoteDevice {
        member: String,
    },
    RemoveRemoteDevice {
        member: String,
    },
    /// On a device: `member`, which offered remote play, is our own PC (the
    /// codes matched). Until then its offer only asks this, and no PIN goes
    /// to it.
    AcceptRemoteHost {
        member: String,
    },
    ForgetRemoteHost {
        member: String,
    },
    /// On a device: unlocks the home PC `member`, met in `room`, with the
    /// PIN. Kept in memory for this run, to unlock again after a reconnect.
    UnlockRemote {
        room: String,
        member: String,
        pin: String,
    },
    /// On a device: locks it again, forgetting the PIN.
    LockRemote {
        room: String,
        member: String,
    },
    /// On a device, unlocked: one of the home PC's own host commands, run
    /// there as if from its window.
    Remote {
        room: String,
        member: String,
        action: RemoteAction,
    },
}

/// What a device may ask its home PC to do (M25): the host's console, and
/// nothing else. Named as the [`Command`]s they stand for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum RemoteAction {
    ListSources,
    /// In the room the command came through. The home PC's own controllers
    /// stay out of the seats: nobody is there.
    StartSession {
        title: String,
        max_viewers: u8,
        input: InputSetup,
    },
    StartStream {
        source: SourceChoice,
        name: String,
        fps: u32,
        max_mbps: u32,
        upload_mbps: Option<u32>,
    },
    StopStream,
    SetStreamLevel {
        percent: u16,
    },
    EndSession,
    AnswerControl {
        member: String,
        accept: bool,
    },
    PassControl {
        member: String,
        seat: u8,
        #[serde(default)]
        #[ts(optional)]
        controller: Option<u8>,
    },
    TakeBackControl {
        seat: Option<u8>,
    },
    PlaySeat {
        seat: u8,
        controller: u8,
    },
    AllowControl,
    StopWaitingForPads,
    /// Desktop control of the home PC (M25b), for this device: its keyboard
    /// and mouse reach the whole desktop through the session's keyboard
    /// seat, or stop.
    Desktop {
        on: bool,
    },
    /// The window the home PC streams, to the front (M25b).
    BringToFront,
    /// Drawing on the stream (M27a), as the home PC's own commands.
    SetDrawing {
        on: bool,
    },
    ClearBoard,
    StopDrawing {
        member: String,
        stopped: bool,
    },
    /// Pictures on the board allowed or not (M27e).
    SetPictures {
        on: bool,
    },
}

impl RemoteAction {
    /// Its name alone, for logs: a stream's name is a window's title.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ListSources => "ListSources",
            Self::StartSession { .. } => "StartSession",
            Self::StartStream { .. } => "StartStream",
            Self::StopStream => "StopStream",
            Self::SetStreamLevel { .. } => "SetStreamLevel",
            Self::EndSession => "EndSession",
            Self::AnswerControl { .. } => "AnswerControl",
            Self::PassControl { .. } => "PassControl",
            Self::TakeBackControl { .. } => "TakeBackControl",
            Self::PlaySeat { .. } => "PlaySeat",
            Self::AllowControl => "AllowControl",
            Self::StopWaitingForPads => "StopWaitingForPads",
            Self::Desktop { .. } => "Desktop",
            Self::BringToFront => "BringToFront",
            Self::SetDrawing { .. } => "SetDrawing",
            Self::ClearBoard => "ClearBoard",
            Self::StopDrawing { .. } => "StopDrawing",
            Self::SetPictures { .. } => "SetPictures",
        }
    }
}

/// What the app asks of the stream window (P11), for watching from another
/// monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum StreamWindowRequest {
    Fullscreen {
        on: bool,
    },
    /// The session bar alone: the centre card and the stats stay.
    ShowBar {
        on: bool,
    },
    ShowStats {
        on: bool,
    },
    /// Come to the front, restored if minimized.
    Raise,
}

impl From<StreamWindowRequest> for couch_proto::ipc::WindowRequest {
    fn from(request: StreamWindowRequest) -> Self {
        use couch_proto::ipc::WindowRequest as W;
        match request {
            StreamWindowRequest::Fullscreen { on } => W::Fullscreen(on),
            StreamWindowRequest::ShowBar { on } => W::ShowBar(on),
            StreamWindowRequest::ShowStats { on } => W::ShowStats(on),
            StreamWindowRequest::Raise => W::Raise,
        }
    }
}

/// Answer to [`Command::GetStreamWindow`]: Settings → Stream window (P11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StreamWindowSettingsView {
    /// New stream windows show the session bar over the picture.
    pub bar: bool,
}

/// Answer to [`Command::GetDrawingDefault`] (M27a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrawingSettingsView {
    /// Sessions start with drawing on.
    pub on: bool,
    /// ...and with pictures allowed (M27e).
    pub pictures: bool,
}

/// Answer to [`Command::GetStreaming`] (M29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StreamingSettingsView {
    /// A monitor in HDR is captured in FP16 and converted to SDR with its
    /// SDR white level; off, Windows' own 8-bit capture (HDR content
    /// clipped at 80 nits).
    pub hdr: bool,
}

/// Answer to [`Command::GetTray`] (M25c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TrayView {
    /// The app keeps an icon in the notification area, and closing its
    /// window hides it there.
    pub on: bool,
}

/// The stream window of a session we watch, for the app's controls (P11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StreamWindowView {
    /// 0 to 100%.
    pub volume: u8,
    pub muted: bool,
    /// The window has said how it is: until then, the controls wait.
    pub open: bool,
    pub fullscreen: bool,
    /// Its session bar shows when anything happens.
    pub bar: bool,
    pub stats: bool,
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

/// One of the drawing editor's keys (M27a), as Settings lists it: rows that
/// do the same thing come together ("Ctrl+Shift+Z or Ctrl+Y").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrawingKeyView {
    pub keys: String,
    pub action: String,
}

impl DrawingKeyView {
    /// Every drawing key, from `couch_input::shortcuts::DRAWING_KEYS`.
    pub fn all() -> Vec<Self> {
        let mut rows: Vec<Self> = Vec::new();
        for key in couch_input::shortcuts::DRAWING_KEYS {
            if key.description.is_empty() {
                if let Some(last) = rows.last_mut() {
                    last.keys = format!("{}, {}", last.keys, key.keys());
                }
            } else {
                rows.push(Self {
                    keys: key.keys(),
                    action: key.description.to_string(),
                });
            }
        }
        rows
    }
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

/// Answer to [`Command::GetNetwork`] and the relay commands (M26a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NetworkView {
    /// The relays this profile holds, the build's own first; empty where
    /// they are fixed ([`Self::changeable`] false).
    pub relays: Vec<RelayView>,
    /// iroh's public relays are in use: nothing is registered, or the user
    /// keeps them on beside ours.
    pub public_in_use: bool,
    /// The user keeps iroh's public relays on beside ours.
    pub keep_public: bool,
    /// The user lets iroh's public relays stand in while none of ours works
    /// (off by default).
    pub public_fallback: bool,
    /// They stand in now: none of our relays works.
    pub falling_back: bool,
    /// iroh's public relays as one row, while they are in use.
    pub public: Option<RelayStateView>,
    /// The relay we are reachable through now; `None` until connected.
    pub home_relay: Option<String>,
    /// The network this PC is on.
    pub internet: InternetView,
    /// Direct connections are possible (UDP works); `false`: everything
    /// goes through a relay, with more delay and a lower picture. `None`
    /// until known.
    pub udp: Option<bool>,
    /// What silently breaks every room, once it has lasted: the shell's
    /// strip and the tray say it.
    pub trouble: Option<NetworkTrouble>,
    /// Plain http relays at private (LAN) addresses are allowed: a
    /// development or test build.
    pub test_build: bool,
    /// Relays can be added and removed here (the app); not where they were
    /// fixed when the engine started (tests).
    pub changeable: bool,
    /// Only iroh's public relays are in use and the user hasn't put the
    /// question off: creating a room or an invite asks for a relay of their
    /// own first (the wizard; [`Command::DeferRelay`]).
    pub ask_for_relay: bool,
}

/// Who reaches us through a relay ([`Command::RelayUse`]), for the warning
/// before removing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RelayUseView {
    /// The rooms with members on it.
    pub rooms: Vec<RelayUseRoom>,
    /// Remote play is on: a device of the user's may be cut off from this
    /// PC, with nobody here to fix it.
    pub remote_play: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RelayUseRoom {
    pub room: String,
    pub name: String,
    /// The members whose record names the relay, or whose connection with
    /// us runs through it now, by name.
    pub members: Vec<String>,
}

/// What a pasted relay code holds, before the user adds it (M26a). Never
/// the token itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RelayCodeView {
    pub url: String,
    pub host: String,
    /// It carries a token; a code for a relay open to everyone doesn't.
    pub has_code: bool,
    /// Given to this user alone: never passed on.
    pub personal: bool,
    /// The relay is in our list already: adding gives it this code.
    pub held: bool,
    /// ... with this very code: adding changes nothing.
    pub same_code: bool,
}

/// Why a relay wasn't added (M26a): checked before saving, it didn't let us
/// in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RelayCheckView {
    pub url: String,
    pub host: String,
    pub outcome: RelayCheckOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum RelayCheckOutcome {
    /// The relay refused the code (rotated, revoked, or never its): ask its
    /// owner for a new one. The relay's own words.
    Refused { reason: String },
    /// Nothing let us in: no relay at that address, or it didn't answer
    /// (down for now?). It can be saved anyway. iroh's error, for Details.
    Unreachable { details: Option<String> },
}

/// One relay this profile holds. Its token never leaves the engine: only
/// whether it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RelayView {
    /// As iroh writes it (`https://relay.example.com/`).
    pub url: String,
    /// Its server's name, to show.
    pub host: String,
    /// This build's own relay: it can't be removed.
    pub built_in: bool,
    /// It has a code (a token); a relay open to everyone has none.
    pub has_code: bool,
    /// From a personal code: never passed on.
    pub personal: bool,
    pub state: RelayStateView,
}

/// A window or screen from [`Command::ListSources`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum SourceChoice {
    Window { id: String },
    Screen { id: String },
}

/// One of the host's notices, for [`Command::ShowNotices`] (M16c).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NoticeView {
    /// Stable while it is up: the same id with new text replaces it.
    pub id: String,
    pub title: String,
    pub body: String,
    /// Stays until closed (a request waiting for an answer).
    pub lasting: bool,
}

/// The most notices up at once, and the longest text of one, in bytes.
const MAX_NOTICES: usize = 16;
const MAX_NOTICE_TEXT: usize = 500;

/// Cuts `text` to at most `max` bytes, at a character's edge.
fn clip(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
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
    /// This PC's own controllers that could play through a seat: physical
    /// ones (P9), and other programs' virtual pads proven so (a remapper's,
    /// M22); never one of ours.
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
    /// A remapper's pad has our virtual pads' ids (a real Xbox 360
    /// controller's): the kind tells it from a real one (M22).
    #[serde(default)]
    pub kind: HostControllerKind,
}

/// What one of the host's controllers is (M22).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum HostControllerKind {
    #[default]
    Physical,
    /// Another program's virtual pad, a remapper's (DS4Windows, say):
    /// played through a seat with its remaps, hidden from games by its
    /// device.
    OtherVirtual,
}

impl HostController {
    /// Its name as the host panel shows it: a remapper's pad has a real
    /// controller's ("Xbox 360 Controller"), so it says whose it is.
    pub fn shown_name(&self) -> String {
        match self.kind {
            HostControllerKind::Physical => self.name.clone(),
            HostControllerKind::OtherVirtual => format!("{} (a remapper's)", self.name),
        }
    }

    pub fn choice(&self) -> couch_input::local::ControllerChoice {
        use couch_input::local::{ControllerChoice, ControllerIds};
        ControllerChoice {
            ids: ControllerIds {
                vendor: self.vendor,
                product: self.product,
            },
            kind: match self.kind {
                HostControllerKind::Physical => ControllerKind::Physical,
                HostControllerKind::OtherVirtual => ControllerKind::OtherVirtual,
            },
        }
    }
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

/// How the game takes input, chosen when a session starts (M19: one setup,
/// where single player and multiplayer were two): 0 to 4 controller seats,
/// each its own virtual pad, and keyboard and mouse as a seat of their own
/// or not for guests. No seats at all is a session to watch together (M24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InputSetup {
    pub controllers: u8,
    pub keyboard_mouse: KeyboardMouseSeat,
}

/// Where a session's keyboard and mouse go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum KeyboardMouseSeat {
    /// Guests get none.
    None,
    /// A seat of their own, after the controllers. One player can hold it
    /// beside a controller seat (M18), which is why "with Controller 1" went
    /// (M24).
    OwnSeat,
}

impl Default for InputSetup {
    /// What guests get unless the host says otherwise: a controller.
    fn default() -> Self {
        Self {
            controllers: 1,
            keyboard_mouse: KeyboardMouseSeat::None,
        }
    }
}

impl InputSetup {
    /// The seats, by seat id: the controllers first, then a keyboard seat;
    /// none for a session to watch together. Not checked:
    /// `couch_room::session` refuses a bad setup (over 4 controllers).
    pub fn seats(&self) -> Vec<SeatKind> {
        let mut seats = vec![SeatKind::Controller; usize::from(self.controllers)];
        if self.keyboard_mouse == KeyboardMouseSeat::OwnSeat {
            seats.push(SeatKind::KeyboardMouse);
        }
        seats
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
    /// For the host: which of their controllers drives it (M23), by its
    /// place in [`SessionView::host_controllers`]; set while they play it.
    pub host_controller: Option<u8>,
    /// For the host, a pad seat a viewer holds: which of the viewer's
    /// controllers drives it (P12), by its slot
    /// ([`GuestControllerView::slot`]), as the viewer last said; `None`
    /// while none does, or before it has said.
    pub guest_controller: Option<u8>,
}

/// One of a viewer's controllers, for the host's patch bay (P12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GuestControllerView {
    /// Its slot in the viewer's stream window, kept while connected.
    pub slot: u8,
    /// As the viewer's system names it ("DualSense Wireless Controller").
    pub name: String,
}

/// A viewer asking for a seat, for the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RequestView {
    pub member: String,
    pub seat: u8,
    /// Which of their controllers asks, by slot (P13): it plays the seat,
    /// leaving the one it drives if they have no controller to spare.
    pub controller: Option<u8>,
}

/// One of our controllers, in a session we watch (P13): asking for a seat
/// with two or more names the one that plays it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OurControllerView {
    /// Its slot in our stream window.
    pub slot: u8,
    pub name: String,
    /// The seat it drives now, if any.
    pub seat: Option<u8>,
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
    /// The refresh rate of the screen it is on, if known: the stream gets
    /// no more frames a second than that (Go live warns).
    pub refresh_hz: Option<u32>,
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
    pub refresh_hz: Option<u32>,
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
    /// For the host: how many controllers each viewer has, as it said, in
    /// `viewers`' order (M18); empty for others' sessions. A seat with a pad
    /// passed to a viewer is added to theirs while they hold fewer pad seats
    /// than this, otherwise they move; a seat without one is always added.
    pub viewer_controllers: Vec<u8>,
    /// For the host: each viewer's controllers, by slot, in `viewers`'
    /// order (P12); empty for others' sessions. The patch bay shows each as
    /// a plug of its own.
    pub viewer_pads: Vec<Vec<GuestControllerView>>,
    /// For a session we watch: our stream window's controllers, and the
    /// seat each drives (P13); empty otherwise.
    pub our_controllers: Vec<OurControllerView>,
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
    /// For the host: their controllers playing through seats, hidden from
    /// games, by name, if they chose some and they work (M23: several).
    pub host_controllers: Vec<String>,
    /// For the host: each of those controllers' own seat, by controller
    /// (M23), held or not (lent to a viewer, left empty): Play here onto
    /// another controller's own seat swaps the two.
    pub host_homes: Vec<u8>,
    /// For the host: why the controllers they chose don't play through
    /// seats (they play outside the seats instead).
    pub host_controller_problem: Option<String>,
    /// Whether anything is streamed (M19).
    pub stream: StreamStateView,
    /// For the host: what is streamed, while it is.
    pub streaming: Option<StreamView>,
    /// For the host: how loud guests hear the sound, 0 to 400% of the game's
    /// (P7); `None` for others' sessions.
    pub sound_level: Option<u16>,
    /// For the host: the seats whose virtual pads Windows is still setting
    /// up (P10), each taking its seat's input once ready.
    pub pads_setting_up: Vec<u8>,
    /// How long that has taken so far, in seconds, when this view was made.
    pub pads_setting_up_secs: u32,
    /// For the host: the seats with a pad that have none, for the rest of the
    /// session (it couldn't be plugged, or setting it up was given up).
    pub pads_missing: Vec<u8>,
    /// For a viewer: our stream window, for the app's controls (P11).
    pub stream_window: Option<StreamWindowView>,
    /// For a viewer: whether the host's game is in front on their PC, as the
    /// host last said (keyboard and mouse don't reach it while it isn't).
    /// For the host, and a session we run from afar (M25b): whether the
    /// streamed window is, for Bring to front; `None` for a screen.
    pub game_in_front: Option<bool>,
    /// For a viewer: the latest news, as the stream window's bar shows it
    /// ("The host said no, for now"); `None` for none.
    pub note: Option<String>,
    /// For a session of one of our own PCs, unlocked (M25): we run it from
    /// here, and the host's fields above are filled as the host sees them.
    pub operator: bool,
    /// For the host: our devices running this session from afar, by name.
    pub operated_by: Vec<String>,
    /// For the host, and a session we run from afar: the member controlling
    /// the host's desktop (M25b), by id. The host's banner names its device.
    pub desktop: Option<String>,
    /// For the host, and a session we run from afar: whether guests may
    /// hold its keyboard-and-mouse seat. Not on a Linux host without a helper
    /// telling which window is in front (M16c-2), where the seat is desktop
    /// control's alone: the console offers it to nobody else (the engine
    /// refuses anyway). True where unknown.
    pub guests_keyboard: bool,
    /// For the host: keyboard and mouse wait for us to name the game's
    /// window among the desktop's (Linux, M16c-2): the session has a
    /// keyboard seat, and a stream runs on a shared screen.
    pub names_game: bool,
    /// For the host: the game's window we named for this stream, if any.
    pub game_window: Option<GameWindowView>,
    /// For the host, while it names the game: its desktop opens a view from
    /// a screen corner that would take guests' keys unseen (KDE's Overview),
    /// so the console asks it to turn the corner off (M16c-2).
    pub warn_hot_corner: bool,
    /// Drawing on the stream (M27a): for the host and a session we run from
    /// afar, the board as the console shows it; for a viewer, whether the
    /// host lets us draw (no counts).
    pub drawing: DrawingView,
    /// Our own session only: the board over the game at this PC (M27b).
    pub board_overlay: Option<BoardOverlayView>,
    /// Our own session only: the host's own stream window (M27d).
    pub own_window: Option<OwnWindowView>,
}

/// The host's own stream window (M27d), as the console shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OwnWindowView {
    /// It is open.
    pub open: bool,
    /// The host draws in it now.
    pub drawing: bool,
    /// It is left out of what is streamed (Windows). Where it can't be (a
    /// Linux host's share), the console says to keep it off the screen
    /// being shared.
    pub left_out: bool,
}

/// The host's board overlay over the game (M27b), as the console shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BoardOverlayView {
    /// This PC can show drawings over the game (Windows).
    pub available: bool,
    /// The host's switch: drawings shown over the game.
    pub shown: bool,
    /// Drawing over the game now: the board has the mouse and the focus.
    pub drawing: bool,
    /// The host's drawing key, such as "Ctrl+Alt+Shift+D"; `None` if it
    /// couldn't be hooked.
    pub key: Option<String>,
    /// Why the drawing key last did nothing.
    pub note: Option<String>,
}

/// Drawing on the stream, as the console shows it (M27a).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrawingView {
    /// The host lets those watching draw.
    pub on: bool,
    /// Elements on the board.
    pub elements: u32,
    /// Each member with something on the board, by id, and how many.
    pub drawers: Vec<DrawerView>,
    /// Members the host stopped drawing, by id.
    pub stopped: Vec<String>,
    /// The host lets them put pictures on the board (M27e).
    pub pictures: bool,
}

/// One member's share of the board (M27a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrawerView {
    pub member: String,
    pub elements: u32,
}

impl DrawingView {
    /// The console's view of a board, from the console state.
    pub(crate) fn of(d: &couch_proto::remote::RemoteDrawing) -> Self {
        Self {
            on: d.on,
            pictures: d.pictures,
            elements: d.elements,
            drawers: d
                .drawers
                .iter()
                .map(|(m, n)| DrawerView {
                    member: m.to_hex(),
                    elements: *n,
                })
                .collect(),
            stopped: d
                .stopped
                .iter()
                .map(couch_proto::ids::MemberId::to_hex)
                .collect(),
        }
    }
}

/// Whether a session streams anything (M19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "state", content = "reason", rename_all = "camelCase")]
#[ts(export)]
pub enum StreamStateView {
    /// Nothing streamed yet in this session.
    Waiting,
    /// A stream is starting (the host's own view only).
    Starting,
    Live,
    /// The last stream ended, and why; the session goes on.
    Stopped(SessionEnd),
}

/// For the host: the stream running in our session (M19). Its Debug leaves
/// the name out: a window's title may say anything.
#[derive(Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StreamView {
    /// The window's title or the screen's name, as the panel named it.
    pub name: String,
    pub fps: u32,
    pub max_mbps: u32,
    pub upload_mbps: Option<u32>,
    /// A screen rather than a window (M25b): nothing to bring to the front.
    pub screen: bool,
}

impl fmt::Debug for StreamView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamView")
            .field("name", &"..")
            .field("fps", &self.fps)
            .field("max_mbps", &self.max_mbps)
            .field("upload_mbps", &self.upload_mbps)
            .field("screen", &self.screen)
            .finish()
    }
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
    /// On Linux (M16c): this user may make input devices (`/dev/uinput`), for
    /// guests' virtual controllers. Not a driver: one udev rule.
    Uinput,
    /// On GNOME (M16c-2): our Shell extension, which tells the app which
    /// window is in front, so guests may have keyboard and mouse. Not a
    /// driver: files in the user's own extensions folder.
    GnomeExtension,
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
    /// Installed, but GNOME reads its extensions only at log-in (M16c-2).
    NeedsLogout,
    /// Installed, and turned off: Install turns it on.
    Off,
    /// Installed, but GNOME's extensions are all turned off (its Extensions
    /// app).
    ExtensionsOff,
    /// Installed, but this GNOME can't run it.
    Unsupported,
    /// An older version is installed: Install updates it (P18).
    Outdated,
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

/// A window the host could name as its stream's game (Linux, M16c-2:
/// [`Command::GameWindows`]). Its `Debug` leaves out the title.
#[derive(Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GameWindowView {
    pub id: u32,
    pub title: String,
    /// Its program.
    pub app: String,
    /// It has the size of what is shared: likely the game.
    pub matches: bool,
}

impl fmt::Debug for GameWindowView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GameWindowView")
            .field("id", &self.id)
            .field("title", &"..")
            .field("app", &self.app)
            .field("matches", &self.matches)
            .finish()
    }
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
    /// Nothing was chosen in the host's screen-sharing dialog (M16c).
    NothingShared,
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
    Pong {
        nonce: u32,
    },
    Profile(ProfileView),
    Rooms {
        rooms: Vec<RoomView>,
    },
    Room(RoomView),
    Invite {
        link: String,
    },
    InvitePreview(InvitePreview),
    OpenedInvite {
        link: Option<String>,
    },
    Sources(SourceList),
    /// The programs playing sound now, by name (M16c), sorted.
    SoundSources {
        programs: Vec<String>,
    },
    Shortcuts {
        shortcuts: Vec<ShortcutView>,
        /// The drawing editor's own keys (M27a): fixed, listed only.
        #[serde(rename = "drawingKeys")]
        #[ts(rename = "drawingKeys")]
        drawing_keys: Vec<DrawingKeyView>,
    },
    Network(NetworkView),
    /// What a pasted relay code holds ([`Command::ReadRelayCode`]).
    RelayCode(RelayCodeView),
    /// A relay wasn't added: it didn't let us in with its code
    /// ([`Command::AddRelay`], [`Command::AddRelayCode`]).
    RelayCheck(RelayCheckView),
    /// Who reaches us through a relay ([`Command::RelayUse`]).
    RelayUse(RelayUseView),
    /// [`Command::GetDiagnostics`]: plain text, to copy.
    Diagnostics {
        text: String,
    },
    StreamWindow(StreamWindowSettingsView),
    Tray(TrayView),
    Drawing(DrawingSettingsView),
    Streaming(StreamingSettingsView),
    Drivers {
        drivers: Vec<DriverView>,
    },
    GameWindows {
        windows: Vec<GameWindowView>,
    },
    SharedChoice {
        remembered: bool,
    },
    FullScreenGame {
        on: bool,
    },
    Notices {
        text: String,
    },
    /// A backup written or restored, at `path`; `None` if the user closed
    /// the dialog.
    Backup {
        path: Option<String>,
    },
    RemotePlay(RemotePlayView),
    Done,
}

/// Remote play (M25), as [`Command::GetRemotePlay`] shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemotePlayView {
    /// This PC lets its devices in.
    pub on: bool,
    pub pin_set: bool,
    /// This profile's own code, to compare when a home PC adds it.
    pub my_code: String,
    /// The devices this PC trusts.
    pub devices: Vec<RemoteDeviceView>,
    /// Members of our rooms that could be added, ourselves and devices
    /// already added left out.
    pub candidates: Vec<RemoteDeviceView>,
    /// Wrong PINs make the next try wait this long, in seconds.
    pub wait_secs: Option<u32>,
    /// On a device: the home PCs it accepted.
    pub hosts: Vec<RemoteDeviceView>,
    /// The device controlling this PC's desktop now, by name (M25b): adding
    /// a device and changing the PIN wait until it ends.
    pub desktop: Option<String>,
    /// Whether this app starts when the user signs in (M25c; Linux: an AppImage);
    /// `None` where it isn't offered (a development build, another profile).
    pub start_at_sign_in: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoteDeviceView {
    pub id: String,
    pub name: String,
    /// Its code, "AB12 CD34": the same on both ends.
    pub code: String,
    /// Connected now, in some room.
    pub online: bool,
}

/// For a device: a member that offered it remote play (M25).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoteAccessView {
    /// We accepted it as our own PC; until then it only asks.
    pub accepted: bool,
    pub unlocked: bool,
    /// Its code, to compare before accepting.
    pub code: String,
    /// Wrong PINs make the next try wait this long, in seconds.
    pub wait_secs: Option<u32>,
    /// Unlocked, and that PC shows its lock screen (M25c): nothing gets in
    /// until someone unlocks it there.
    pub locked: bool,
    /// Unlocked: what that PC can host, as it said (M16c). The console hides
    /// the keyboard seat and desktop control where they're missing.
    pub capabilities: Option<HostCapabilities>,
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
            Self::AddRelay {
                url,
                token,
                personal,
                unchecked,
            } => f
                .debug_struct("AddRelay")
                .field("url", url)
                .field("token", &token.as_ref().map(|_| ".."))
                .field("personal", personal)
                .field("unchecked", unchecked)
                .finish(),
            Self::CheckNetwork => f.write_str("CheckNetwork"),
            Self::SetPublicFallback { on } => {
                f.debug_struct("SetPublicFallback").field("on", on).finish()
            }
            Self::ReadRelayCode { .. } => f.write_str("ReadRelayCode { .. }"),
            Self::AddRelayCode { unchecked, .. } => f
                .debug_struct("AddRelayCode")
                .field("code", &"..")
                .field("unchecked", unchecked)
                .finish(),
            Self::ShareRelay { url } => f.debug_struct("ShareRelay").field("url", url).finish(),
            Self::RemoveRelay { url } => f.debug_struct("RemoveRelay").field("url", url).finish(),
            Self::SetPublicRelays { on } => {
                f.debug_struct("SetPublicRelays").field("on", on).finish()
            }
            Self::RelayUse { url } => f.debug_struct("RelayUse").field("url", url).finish(),
            Self::DeferRelay { never } => {
                f.debug_struct("DeferRelay").field("never", never).finish()
            }
            Self::GetDiagnostics => f.write_str("GetDiagnostics"),
            Self::OpenLogFolder => f.write_str("OpenLogFolder"),
            Self::GetDrivers => f.write_str("GetDrivers"),
            Self::SharedChoice { forget } => f
                .debug_struct("SharedChoice")
                .field("forget", forget)
                .finish(),
            Self::PlayChime => f.write_str("PlayChime"),
            Self::FullScreenGame => f.write_str("FullScreenGame"),
            Self::GetNotices => f.write_str("GetNotices"),
            // The passphrase opens the whole identity.
            Self::ExportProfile { .. } => f.write_str("ExportProfile { .. }"),
            Self::RestoreProfile { .. } => f.write_str("RestoreProfile { .. }"),
            Self::StopCatchingUp { room } => f
                .debug_struct("StopCatchingUp")
                .field("room", room)
                .finish(),
            Self::OfferRoom { room, member } => f
                .debug_struct("OfferRoom")
                .field("room", room)
                .field("member", member)
                .finish(),
            Self::WithdrawOffer { room } => {
                f.debug_struct("WithdrawOffer").field("room", room).finish()
            }
            // The new password: newcomers' half of the way in.
            Self::AnswerOffer { room, accept, .. } => f
                .debug_struct("AnswerOffer")
                .field("room", room)
                .field("accept", accept)
                .finish_non_exhaustive(),
            Self::InstallDriver { driver } => f
                .debug_struct("InstallDriver")
                .field("driver", driver)
                .finish(),
            Self::ListSources => f.write_str("ListSources"),
            Self::ListSoundSources => f.write_str("ListSoundSources"),
            Self::ShowNotices { notices } => f
                .debug_struct("ShowNotices")
                .field("notices", &notices.len())
                .finish(),
            Self::StartSession {
                room,
                max_viewers,
                input,
                controllers,
                ..
            } => f
                .debug_struct("StartSession")
                .field("room", room)
                .field("max_viewers", max_viewers)
                .field("input", input)
                .field("controllers", controllers)
                .finish_non_exhaustive(),
            // Never the window's title: it may say anything.
            Self::StartStream {
                fps,
                max_mbps,
                upload_mbps,
                ..
            } => f
                .debug_struct("StartStream")
                .field("fps", fps)
                .field("max_mbps", max_mbps)
                .field("upload_mbps", upload_mbps)
                .finish_non_exhaustive(),
            Self::StopStream => f.write_str("StopStream"),
            Self::GameWindows => f.write_str("GameWindows"),
            Self::SetGameWindow { id } => f.debug_struct("SetGameWindow").field("id", id).finish(),
            Self::SetStreamLevel { percent } => f
                .debug_struct("SetStreamLevel")
                .field("percent", percent)
                .finish(),
            Self::EndSession => f.write_str("EndSession"),
            Self::Watch { room, host } => f
                .debug_struct("Watch")
                .field("room", room)
                .field("host", host)
                .finish(),
            Self::StopWatching => f.write_str("StopWatching"),
            Self::RequestControl { seat, controller } => f
                .debug_struct("RequestControl")
                .field("seat", seat)
                .field("controller", controller)
                .finish(),
            Self::CancelControl => f.write_str("CancelControl"),
            Self::GiveBackSeat { seat } => {
                f.debug_struct("GiveBackSeat").field("seat", seat).finish()
            }
            Self::AnswerControl { member, accept } => f
                .debug_struct("AnswerControl")
                .field("member", member)
                .field("accept", accept)
                .finish(),
            Self::PassControl {
                member,
                seat,
                controller,
            } => f
                .debug_struct("PassControl")
                .field("member", member)
                .field("seat", seat)
                .field("controller", controller)
                .finish(),
            Self::TakeBackControl { seat } => f
                .debug_struct("TakeBackControl")
                .field("seat", seat)
                .finish(),
            Self::PlaySeat { seat, controller } => f
                .debug_struct("PlaySeat")
                .field("seat", seat)
                .field("controller", controller)
                .finish(),
            Self::AllowControl => f.write_str("AllowControl"),
            Self::StopWaitingForPads => f.write_str("StopWaitingForPads"),
            Self::BringToFront => f.write_str("BringToFront"),
            Self::SetDrawing { on } => f.debug_struct("SetDrawing").field("on", on).finish(),
            Self::ClearBoard => f.write_str("ClearBoard"),
            Self::StopDrawing { member, stopped } => f
                .debug_struct("StopDrawing")
                .field("member", member)
                .field("stopped", stopped)
                .finish(),
            Self::HostDraw { generation, ops } => f
                .debug_struct("HostDraw")
                .field("generation", generation)
                .field("ops", &ops.len())
                .finish(),
            Self::HostBoard => f.write_str("HostBoard"),
            Self::SetPictures { on } => f.debug_struct("SetPictures").field("on", on).finish(),
            Self::HostPicture {
                generation, data, ..
            } => f
                .debug_struct("HostPicture")
                .field("generation", generation)
                .field("data", &data.len())
                .finish(),
            Self::EndHostDrawing => f.write_str("EndHostDrawing"),
            Self::ShowBoardOverlay { on } => {
                f.debug_struct("ShowBoardOverlay").field("on", on).finish()
            }
            Self::OwnStreamWindow { open } => f
                .debug_struct("OwnStreamWindow")
                .field("open", open)
                .finish(),
            Self::GetDrawingDefault => f.write_str("GetDrawingDefault"),
            Self::SetDrawingDefault { on, pictures } => f
                .debug_struct("SetDrawingDefault")
                .field("on", on)
                .field("pictures", pictures)
                .finish(),
            Self::GetStreaming => f.write_str("GetStreaming"),
            Self::SetStreaming { hdr } => f.debug_struct("SetStreaming").field("hdr", hdr).finish(),
            Self::SetStreamVolume { percent, muted } => f
                .debug_struct("SetStreamVolume")
                .field("percent", percent)
                .field("muted", muted)
                .finish(),
            Self::StreamWindow { request } => f
                .debug_struct("StreamWindow")
                .field("request", request)
                .finish(),
            Self::GetStreamWindow => f.write_str("GetStreamWindow"),
            Self::GetTray => f.write_str("GetTray"),
            Self::SetTray { on } => f.debug_struct("SetTray").field("on", on).finish(),
            Self::SetStreamWindow { bar } => {
                f.debug_struct("SetStreamWindow").field("bar", bar).finish()
            }
            Self::GetRemotePlay => f.write_str("GetRemotePlay"),
            Self::SetRemotePlay { on } => f.debug_struct("SetRemotePlay").field("on", on).finish(),
            Self::SetStartAtSignIn { on } => {
                f.debug_struct("SetStartAtSignIn").field("on", on).finish()
            }
            // The PIN guards the whole PC.
            Self::SetRemotePin { .. } => f.write_str("SetRemotePin { .. }"),
            Self::AddRemoteDevice { member } => f
                .debug_struct("AddRemoteDevice")
                .field("member", member)
                .finish(),
            Self::RemoveRemoteDevice { member } => f
                .debug_struct("RemoveRemoteDevice")
                .field("member", member)
                .finish(),
            Self::AcceptRemoteHost { member } => f
                .debug_struct("AcceptRemoteHost")
                .field("member", member)
                .finish(),
            Self::ForgetRemoteHost { member } => f
                .debug_struct("ForgetRemoteHost")
                .field("member", member)
                .finish(),
            Self::UnlockRemote { room, member, .. } => f
                .debug_struct("UnlockRemote")
                .field("room", room)
                .field("member", member)
                .finish_non_exhaustive(),
            Self::LockRemote { room, member } => f
                .debug_struct("LockRemote")
                .field("room", room)
                .field("member", member)
                .finish(),
            Self::Remote {
                room,
                member,
                action,
            } => f
                .debug_struct("Remote")
                .field("room", room)
                .field("member", member)
                .field("action", &action.kind())
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
            Self::SoundSources { programs } => f
                .debug_struct("SoundSources")
                .field("programs", &programs.len())
                .finish(),
            Self::Shortcuts { shortcuts, .. } => f
                .debug_struct("Shortcuts")
                .field("shortcuts", &shortcuts.len())
                .finish_non_exhaustive(),
            Self::Network(network) => f.debug_tuple("Network").field(network).finish(),
            Self::RelayCode(code) => f.debug_tuple("RelayCode").field(code).finish(),
            Self::RelayCheck(check) => f.debug_tuple("RelayCheck").field(check).finish(),
            Self::RelayUse(used) => f.debug_tuple("RelayUse").field(used).finish(),
            Self::Diagnostics { text } => f
                .debug_struct("Diagnostics")
                .field("bytes", &text.len())
                .finish(),
            Self::StreamWindow(settings) => f.debug_tuple("StreamWindow").field(settings).finish(),
            Self::Tray(tray) => f.debug_tuple("Tray").field(tray).finish(),
            Self::Drawing(d) => f.debug_tuple("Drawing").field(d).finish(),
            Self::Streaming(s) => f.debug_tuple("Streaming").field(s).finish(),
            Self::Drivers { drivers } => {
                f.debug_struct("Drivers").field("drivers", drivers).finish()
            }
            Self::GameWindows { windows } => f
                .debug_struct("GameWindows")
                .field("windows", windows)
                .finish(),
            Self::SharedChoice { remembered } => f
                .debug_struct("SharedChoice")
                .field("remembered", remembered)
                .finish(),
            Self::FullScreenGame { on } => {
                f.debug_struct("FullScreenGame").field("on", on).finish()
            }
            Self::Notices { text } => f
                .debug_struct("Notices")
                .field("bytes", &text.len())
                .finish(),
            // The path may name the user.
            Self::Backup { path } => f
                .debug_struct("Backup")
                .field("path", &path.as_ref().map(|_| ".."))
                .finish(),
            Self::RemotePlay(view) => f.debug_tuple("RemotePlay").field(view).finish(),
            Self::Done => f.write_str("Done"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EngineInfo {
    pub app_version: String,
    /// The commit this build was made from (an installer's), or `None` for
    /// a development build.
    pub commit: Option<String>,
    /// Where the app's source is published (`Cargo.toml`'s `repository`),
    /// once it is.
    pub repository: Option<String>,
    pub protocol: String,
    pub profile: String,
    pub data_dir: String,
    /// The system this build runs on.
    pub platform: Platform,
    /// What this system can host (`Backend::capabilities`): the UI hides
    /// Start a session and the home PC's side of remote play without
    /// `sessions` (M16b), and the keyboard seat and desktop control without
    /// theirs (M16c).
    pub capabilities: HostCapabilities,
    /// Whether this PC can be a home PC, run from afar (`Backend::home_pc`):
    /// Settings offers the home PC's side of remote play only then (a Linux
    /// host until M16c-1's fifth part doesn't).
    pub home_pc: bool,
}

/// What a system can host (M16c, ADR 0012): Windows everything; Linux
/// sessions since M16c-1, with controller seats for guests, desktop control
/// since M16c-2's first part once `/dev/uinput` is writable, and the
/// keyboard seat for guests since its second part while also the desktop's
/// helper tells which window is in front; elsewhere nothing.
/// Each is refused before anything is set up or announced, whatever the UI
/// shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HostCapabilities {
    /// Sessions at all, and the home PC's side of remote play.
    pub sessions: bool,
    /// A keyboard-and-mouse seat for guests.
    pub keyboard_seat: bool,
    /// Desktop control for a device of ours (M25b).
    pub desktop_control: bool,
}

impl HostCapabilities {
    pub const ALL: Self = Self {
        sessions: true,
        keyboard_seat: true,
        desktop_control: true,
    };
}

impl From<HostCapabilities> for couch_proto::remote::HostCapabilities {
    fn from(c: HostCapabilities) -> Self {
        Self {
            sessions: c.sessions,
            keyboard_seat: c.keyboard_seat,
            desktop_control: c.desktop_control,
        }
    }
}

impl From<couch_proto::remote::HostCapabilities> for HostCapabilities {
    fn from(c: couch_proto::remote::HostCapabilities) -> Self {
        Self {
            sessions: c.sessions,
            keyboard_seat: c.keyboard_seat,
            desktop_control: c.desktop_control,
        }
    }
}

/// The system this build runs on (M16b). `SystemPlatform` in TypeScript,
/// beside the front end's own `Platform` (its window onto Tauri).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename = "SystemPlatform")]
pub enum Platform {
    Windows,
    Linux,
    Other,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
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
    /// Grows with every view the engine makes, of any room: of two views of
    /// one room, the higher is newer. A command's reply and a `RoomChanged`
    /// event reach the UI by different paths, in either order, so the UI
    /// keeps the newer one (a newcomer's join reply once hid a session
    /// announced just after it, 2026-10-18).
    #[ts(type = "number")]
    pub revision: u64,
    pub name: String,
    pub is_owner: bool,
    /// This profile first, then by name.
    pub members: Vec<MemberView>,
    /// For the owner: while set, newcomers can't join, after too many wrong
    /// passwords. Seconds left.
    pub joins_paused_for_secs: Option<u32>,
    /// Live sessions: ours, and those of members connected to us.
    pub sessions: Vec<SessionView>,
    /// Restored from a backup and not caught up with the members yet
    /// (M13): no inviting, admitting or banning until they have been
    /// online (a member's room: until one of them has), or the owner stops
    /// waiting ([`Command::StopCatchingUp`]). Leaving is allowed.
    pub catching_up: bool,
    /// While catching up, the members not heard from yet: the owner waits
    /// for all of them, a member for any one.
    pub waiting_for: Vec<String>,
    /// For the owner: the room has no password (M13: restored from a
    /// backup, which never carries one, or handed to us before we chose
    /// one); set a new one to invite again.
    pub needs_password: bool,
    /// The member we offered the room to, while the offer stands (M13).
    pub offering_to: Option<String>,
    /// The owner offers us the room: whether we accepted already (M13).
    pub offered: Option<bool>,
    /// We own the room and can offer it to a member now (M13): not after a
    /// restore until confirmed, not with an offer standing, not past the
    /// 32 handovers a room's chain holds.
    pub can_offer: bool,
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
    /// They offered us remote play (M25): one of our own PCs, or asking to
    /// be.
    pub remote: Option<RemoteAccessView>,
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
    /// While we stream sound: how loud guests hear it, after our level
    /// (P7), four times a second. Peak and average, in whole dB below full
    /// scale (-90 is silence).
    StreamLevel {
        room: String,
        peak_db: i8,
        average_db: i8,
    },
    RoomRemoved {
        room: String,
        reason: RoomGone,
    },
    /// Our connection changed (M26a): a relay's state, the network's, or
    /// the trouble the shell's strip and the tray show.
    Network(NetworkView),
    /// Too many wrong passwords: newcomers can't join our room for a while.
    JoinsPaused {
        room: String,
        minutes: u32,
    },
    /// The room changed hands (M13): `owner` owns it now, perhaps us.
    OwnerChanged {
        room: String,
        owner: String,
    },
    /// The room's owner offers it to us (M13): [`Command::AnswerOffer`].
    RoomOffered {
        room: String,
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
    /// Our session ended other than by [`Command::EndSession`].
    SessionEnded {
        room: String,
        reason: SessionEnd,
    },
    /// Our stream ended other than by [`Command::StopStream`] (M19): the game
    /// exited, its window or screen went, or the capture failed. The session
    /// goes on.
    StreamEnded {
        room: String,
        reason: SessionEnd,
    },
    /// Watching ended other than by [`Command::StopWatching`].
    WatchEnded {
        room: String,
        host: String,
        reason: WatchEnd,
    },
    /// While hosting: what the board says to our board overlay over the
    /// game (M27b), as a stream window hears it: others' drawings, clears,
    /// our own refusals and resyncs.
    HostBoard {
        room: String,
        news: board::BoardNewsView,
    },
    /// While hosting: a picture on the board, for our board overlay over
    /// the game (M27e), by its id in hex (`BoardElement::picture`): its
    /// bytes in base64, checked as the board took them. Each once while it
    /// is on the board, and every one as the overlay loads.
    HostPicture {
        room: String,
        picture: String,
        data: String,
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
    /// No answer from the owner: its app closed, or (the first dial to an
    /// owner just started) the connection timed out while iroh found a path.
    #[error(
        "couldn't reach the room's owner: invites work while the owner's Couch Co-App is running (try again in a moment if it is)"
    )]
    OwnerOffline,
    #[error("passwords can't be empty")]
    EmptyPassword,
    #[error("this room came back from a backup without its password: set a new one first")]
    NeedsPassword,
    #[error(transparent)]
    Backup(#[from] backup::BackupError),
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
    /// A pasted relay code can't be read (M26a); the text says why.
    #[error(transparent)]
    RelayCode(#[from] relays::code::CodeError),
    #[error("the driver wasn't installed: {0}")]
    DriverInstall(String),
    #[error("the licence notices can't be read: {0}")]
    Notices(String),
    /// The other side's Couch Co-App can't talk to ours (M10a).
    #[error("{}", other_version_text(app_version, *newer))]
    OtherVersion { app_version: String, newer: bool },
    /// Capture, encoding or the viewer window failed; the text says how.
    #[error("{0}")]
    Media(String),
    #[error(transparent)]
    Pin(#[from] remote::PinError),
    /// Remote play (M25): the home PC refused, or couldn't be asked; the
    /// text says why.
    #[error("{0}")]
    Remote(String),
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

/// Where a profile's backup is written or read (M13): the app asks the user
/// in its own dialogs, so the webview never names a path (a script in it
/// could otherwise write the identity anywhere, or read any file). Both
/// block until the user answers; `None` if they closed the dialog.
pub trait BackupFiles: Send + Sync {
    /// Where to write a new backup, offering `suggested` as its name.
    fn save_to(&self, suggested: &str) -> Option<std::path::PathBuf>;
    /// Which backup to restore.
    fn open_from(&self) -> Option<std::path::PathBuf>;
}

/// Where the engine's relays come from (M26a).
#[derive(Debug, Clone)]
pub enum Relays {
    /// The profile's registered relays, changed while running from Settings
    /// (the app).
    Profile(RelayRules),
    /// Exactly these, for the whole run (tests, which also turn them off).
    Fixed(RelayChoice),
}

/// A relay's server name, to show: `relay.example.com` for
/// `https://relay.example.com./`; the address itself if it has none.
fn relay_host(url: &str) -> String {
    url.parse::<couch_net::RelayUrl>()
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_end_matches('.').to_string()))
        .unwrap_or_else(|| url.to_string())
}

/// The relays a running engine holds.
pub(crate) struct RelayState {
    /// What this build allows; `None` where the relays are fixed.
    rules: Option<RelayRules>,
    saved: SavedRelays,
    /// What the endpoint holds now.
    in_use: RelayChoice,
    /// Which relay addresses from others the rooms dial; ours among them.
    dial: Arc<std::sync::RwLock<couch_net::addr::DialRules>>,
    /// A backup was restored: its relays are on disk, taken at the next
    /// start; no change meanwhile.
    frozen: bool,
}

impl RelayState {
    /// Moves the endpoint to the saved relays, less those set aside for
    /// refusing our code (`benched`): iroh picks its home relay by speed
    /// alone and keeps one that refuses us, which would leave us reachable
    /// nowhere while a working relay sits beside it. iroh's public relays
    /// join while `falling_back` (none of ours works, and the user lets
    /// them stand in). Nothing where the relays are fixed.
    pub(crate) async fn apply(
        &mut self,
        endpoint: &couch_net::Endpoint,
        benched: &std::collections::BTreeSet<String>,
        falling_back: bool,
    ) {
        let Some(rules) = &self.rules else {
            return;
        };
        let to = match self.saved.choice(rules) {
            RelayChoice::Relays { ours, public } => RelayChoice::Relays {
                ours: ours
                    .into_iter()
                    .filter(|r| !benched.contains(r.url.as_str()))
                    .collect(),
                public: public || (falling_back && rules.public_relays),
            },
            RelayChoice::Disabled => RelayChoice::Disabled,
        };
        if to != self.in_use {
            couch_net::endpoint::change_relays(endpoint, &self.in_use, &to).await;
            self.in_use = to;
        }
    }

    /// Lists them in the network view (their states come from its watch).
    pub(crate) fn list_into(&self, network: &mut network::Network) {
        network.listed = self
            .saved
            .relays
            .iter()
            .map(|relay| network::Listed {
                view: RelayView {
                    host: relay_host(&relay.url),
                    url: relay.url.clone(),
                    built_in: relay.built_in,
                    has_code: relay.token.is_some(),
                    personal: relay.personal,
                    state: RelayStateView::Checking,
                },
                token: relay
                    .token
                    .clone()
                    .and_then(couch_net::endpoint::RelayToken::new),
            })
            .collect();
        network.public_in_use = matches!(self.in_use, RelayChoice::Relays { public: true, .. });
        network.keep_public = self.saved.keep_public;
        network.public_fallback = self.saved.public_fallback;
        network.public_offered = self.rules.as_ref().is_some_and(|r| r.public_relays);
        network.ask = self.saved.ask.clone();
        // Our own relays are always dialed, a LAN one included.
        let ours = self
            .saved
            .relays
            .iter()
            .filter_map(|r| r.url.parse().ok())
            .collect();
        self.dial
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .ours = ours;
    }
}

/// How the engine runs. Tests shorten the timings and turn relays off.
#[derive(Clone)]
pub struct EngineOptions {
    pub relays: Relays,
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
    /// The app's file dialogs for backups (M13); without them, backups are
    /// refused.
    pub backup_files: Option<Arc<dyn BackupFiles>>,
    /// The app's board overlay window over the game (M27b); without it, the
    /// host sees nothing drawn over the game.
    pub board_windows: Option<Arc<dyn board::BoardWindows>>,
    /// Whether this app may start itself when the user signs in (M25c):
    /// Settings offers it only then.
    pub start_at_sign_in: bool,
    /// How our own connection is watched (M26a).
    pub network: NetworkTimings,
}

impl EngineOptions {
    /// The desktop app on this profile's relays (iroh's public ones while it
    /// has none, as in a development build), with this machine's backend.
    pub fn development() -> Self {
        Self {
            relays: Relays::Profile(RelayRules::of_this_build()),
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
            backup_files: None,
            board_windows: None,
            start_at_sign_in: false,
            network: NetworkTimings::default(),
        }
    }

    /// The desktop app: [`Self::development`] on this profile, its relays
    /// those it registered (Settings → Network), the build's own among them
    /// ([`build_config`]).
    pub fn desktop(profile: &Profile) -> Self {
        if let Some(url) = build_config::unreadable_relay() {
            tracing::error!(
                "this build's relay URL {url:?} doesn't parse; it has no relay of its own"
            );
        }
        Self {
            // An installed app's default profile only (M25c): a development
            // build would start the debug exe, which needs the dev server,
            // and a Run entry can't name another profile. On Linux, an
            // AppImage's (M16c): its autostart entry names the AppImage.
            start_at_sign_in: installed_for_sign_in()
                && !cfg!(debug_assertions)
                && profile.is_default(),
            backend: pipeline::profile_backend(profile.data_dir()),
            ..Self::development()
        }
    }
}

/// Whether this app is installed as something that can start at sign-in:
/// any exe on Windows, an AppImage on Linux.
#[cfg(target_os = "linux")]
fn installed_for_sign_in() -> bool {
    couch_platform_linux::autostart::own_appimage().is_some()
}

#[cfg(not(target_os = "linux"))]
fn installed_for_sign_in() -> bool {
    cfg!(windows)
}

/// The third-party licences, installed next to the app's exe
/// (`cargo xtask notices`, then `stage-dlls`; the installer ships them).
const NOTICES_FILE: &str = "THIRD-PARTY-NOTICES.txt";

fn read_notices(path: &std::path::Path) -> Result<String, EngineError> {
    std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => EngineError::Notices(format!(
                "{} is missing; run `cargo xtask notices`, then `cargo xtask stage-dlls`",
                path.display()
            )),
            _ => EngineError::Notices(e.to_string()),
        })
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
    // Its owner's alone on Unix (0700), even made earlier (by the shell's
    // log, or an older build): nothing inside is then reachable by others,
    // whatever each file's own mode, the webview's data included (M16b).
    store::create_private_dir(profile.data_dir()).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(profile.data_dir(), std::fs::Permissions::from_mode(0o700))
            .map_err(io)?;
    }
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
    /// Seals what the profile keeps on disk (for backups, M13).
    secrets: Arc<dyn secrets::SecretStore>,
    backup_files: Option<Arc<dyn BackupFiles>>,
    /// A backup is being written or restored: one at a time.
    backing_up: AtomicBool,
    member_id: String,
    events: broadcast::Sender<Event>,
    actor: mpsc::Sender<rooms::Input>,
    /// The invite link the app was last opened with, until the UI takes it.
    opened: Mutex<Option<String>>,
    backend: Arc<dyn pipeline::Backend>,
    /// While we stream: releases every seat without waiting for the actor.
    panic: Arc<Mutex<Option<pipeline::host::PanicHandle>>>,
    /// The relays the endpoint holds, and where they come from (M26a): one
    /// change at a time, saved and then applied to the running endpoint.
    relays: Arc<tokio::sync::Mutex<RelayState>>,
    timings: NetworkTimings,
    /// What we can tell about our own connection, kept by its watch.
    network: network::Shared,
    /// Wakes the watch after the relays changed.
    poke: mpsc::Sender<()>,
    endpoint: couch_net::Endpoint,
    /// Held while the shortcut keys are read, changed and written, so two
    /// changes at once can't lose one.
    shortcuts_edit: Mutex<()>,
    /// A driver's setup is running ([`Command::InstallDriver`]).
    installing_driver: AtomicBool,
    /// Whether the app keeps an icon in the notification area (M25c,
    /// `app.json`).
    tray: AtomicBool,
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
        let secrets = options.secret_store.clone();
        let dial = Arc::new(std::sync::RwLock::new(couch_net::addr::DialRules {
            lan: match &options.relays {
                Relays::Profile(rules) => rules.test_build,
                Relays::Fixed(_) => build_config::test_build(),
            },
            ours: Default::default(),
        }));
        let relays = match &options.relays {
            Relays::Profile(rules) => {
                let store = store::Store::new(profile.data_dir(), secrets.clone());
                let saved = store.relays(rules, remote::unix_now());
                RelayState {
                    in_use: saved.choice(rules),
                    rules: Some(rules.clone()),
                    saved,
                    dial: dial.clone(),
                    frozen: false,
                }
            }
            Relays::Fixed(choice) => RelayState {
                rules: None,
                saved: SavedRelays::default(),
                in_use: choice.clone(),
                dial: dial.clone(),
                frozen: false,
            },
        };
        let backup_files = options.backup_files.clone();
        let timings = options.network;
        let tray = store::app(profile.data_dir()).tray;
        let (carrying_tx, carrying) = tokio::sync::watch::channel(Default::default());
        let started = rooms::start(
            &profile,
            options,
            relays.in_use.clone(),
            events.clone(),
            panic.clone(),
            carrying_tx,
            dial,
        )
        .await?;
        let network = Arc::new(Mutex::new(network::Network::new(
            relays
                .rules
                .as_ref()
                .map_or_else(build_config::test_build, |r| r.test_build),
            relays.rules.is_some(),
        )));
        relays.list_into(&mut network::lock(&network));
        let relays = Arc::new(tokio::sync::Mutex::new(relays));
        let (poke, poked) = mpsc::channel(4);
        network::watch(
            network.clone(),
            relays.clone(),
            started.endpoint.clone(),
            carrying,
            poked,
            events.clone(),
            timings,
        );
        Ok(Self {
            profile,
            secrets,
            backup_files,
            backing_up: AtomicBool::new(false),
            member_id: started.member_id.to_hex(),
            events,
            actor: started.inbox,
            opened: Mutex::new(None),
            backend,
            panic,
            relays,
            timings,
            network,
            poke,
            endpoint: started.endpoint,
            shortcuts_edit: Mutex::new(()),
            installing_driver: AtomicBool::new(false),
            tray: AtomicBool::new(tray),
            _lock: lock,
        })
    }

    /// Whether the app keeps an icon in the notification area (M25c): its
    /// window then closes to it, and a start at sign-in stays there. Read
    /// without waiting on the engine.
    pub fn tray_on(&self) -> bool {
        self.tray.load(Ordering::SeqCst)
    }

    /// Changes the profile's relays (M26a): `change` edits them, then they
    /// are saved, and the running endpoint moves to them (our vendored iroh
    /// starts a relay's connection again when its token changes or it
    /// goes). Refused where the relays were fixed at start.
    async fn change_relays(
        &self,
        change: impl FnOnce(&mut RelayState) -> Result<(), EngineError>,
    ) -> Result<Response, EngineError> {
        let mut state = self.relays.lock().await;
        if state.rules.is_none() {
            return Err(EngineError::Refused(
                "this app's relays were fixed when it started",
            ));
        }
        if state.frozen {
            return Err(EngineError::Refused(
                "a backup was restored: start the app again before changing relays",
            ));
        }
        let before = state.saved.clone();
        change(&mut state)?;
        if state.saved != before {
            let store = store::Store::new(self.profile.data_dir(), self.secrets.clone());
            if let Err(e) = store.save_relays(&state.saved) {
                state.saved = before;
                return Err(e.into());
            }
            let benched = {
                let mut network = network::lock(&self.network);
                // What was learned of a relay that came, went, or whose code
                // changed no longer holds: it is tried again.
                let same = |a: &relays::SavedRelay, b: &relays::SavedRelay| {
                    a.url == b.url && a.token == b.token
                };
                for relay in before.relays.iter().chain(&state.saved.relays) {
                    let kept = before.relays.iter().any(|r| same(r, relay))
                        && state.saved.relays.iter().any(|r| same(r, relay));
                    if !kept {
                        network.forget_check(&relay.url);
                    }
                }
                (network.benched.clone(), network.falling_back())
            };
            state.apply(&self.endpoint, &benched.0, benched.1).await;
            state.list_into(&mut network::lock(&self.network));
        }
        drop(state);
        let _ = self.poke.try_send(());
        Ok(self.network_view())
    }

    /// Adds a relay the user gave (typed, or from a code): checked first
    /// unless `unchecked`, outside the lock (it takes up to
    /// [`NetworkTimings::check_timeout`]), and saved only if it let us in.
    async fn add_relay(
        &self,
        url: &str,
        token: Option<couch_net::endpoint::RelayToken>,
        personal: bool,
        unchecked: bool,
    ) -> Result<Response, EngineError> {
        let test_build = match &self.relays.lock().await.rules {
            Some(rules) => rules.test_build,
            None => {
                return Err(EngineError::Refused(
                    "this app's relays were fixed when it started",
                ));
            }
        };
        let url = relays::check_url(url, test_build)?;
        if !unchecked {
            let relay = couch_net::endpoint::Relay::new(url.clone(), token.clone());
            let outcome =
                match couch_net::endpoint::check_relay(&relay, self.timings.check_timeout).await {
                    Ok(couch_net::endpoint::RelayCheck::Accepted) => None,
                    Ok(couch_net::endpoint::RelayCheck::Refused(reason)) => {
                        Some(RelayCheckOutcome::Refused { reason })
                    }
                    Ok(couch_net::endpoint::RelayCheck::Unreachable(details)) => {
                        Some(RelayCheckOutcome::Unreachable { details })
                    }
                    Err(e) => Some(RelayCheckOutcome::Unreachable {
                        details: Some(e.to_string()),
                    }),
                };
            if let Some(outcome) = outcome {
                tracing::info!(%url, ?outcome, "relay not added: the check failed");
                return Ok(Response::RelayCheck(RelayCheckView {
                    host: relay_host(url.as_str()),
                    url: url.to_string(),
                    outcome,
                }));
            }
        }
        self.change_relays(|state| {
            let added = state
                .saved
                .add(url.clone(), token, personal, remote::unix_now())?;
            tracing::info!(%url, ?added, unchecked, "relay registered");
            Ok(())
        })
        .await?;
        if !unchecked {
            // It let us in just now: no need to check it again soon.
            network::lock(&self.network).checked(
                url.as_str(),
                couch_net::endpoint::RelayCheck::Accepted,
                std::time::Instant::now(),
            );
        }
        Ok(self.network_view())
    }

    /// What we can tell about our own connection, now.
    fn network_view(&self) -> Response {
        Response::Network(network::lock(&self.network).view())
    }

    /// The trouble the shell's strip and the tray say now (M26a), read
    /// without waiting on the engine.
    pub fn network_trouble(&self) -> Option<NetworkTrouble> {
        network::lock(&self.network).view().trouble
    }

    /// [`Command::GetDiagnostics`].
    async fn diagnostics(&self) -> Result<Response, EngineError> {
        let rooms = match self.forward(Command::ListRooms).await? {
            Response::Rooms { rooms } => rooms,
            _ => Vec::new(),
        };
        let network = network::lock(&self.network).view();
        let text = diagnostics::text(&diagnostics::Facts {
            app_version: env!("CARGO_PKG_VERSION"),
            commit: build_config::commit(),
            build: if cfg!(debug_assertions) {
                "development build"
            } else if build_config::test_build() {
                "test build"
            } else {
                "release build"
            },
            system: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            wire: couch_proto::PROTOCOL_MINOR,
            network: &network,
            rooms: &rooms,
        });
        Ok(Response::Diagnostics { text })
    }

    /// Runs a backup or restore, one at a time, with the app's dialogs.
    async fn backing_up<F>(
        &self,
        run: impl FnOnce(Arc<dyn BackupFiles>) -> F,
    ) -> Result<Response, EngineError>
    where
        F: Future<Output = Result<Response, EngineError>>,
    {
        let Some(files) = self.backup_files.clone() else {
            return Err(EngineError::Refused("backups need the app's file dialogs"));
        };
        if self.backing_up.swap(true, Ordering::SeqCst) {
            return Err(EngineError::Refused(
                "a backup is already being written or restored",
            ));
        }
        // Let go however this ends, a dropped future included.
        struct Done<'a>(&'a AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _done = Done(&self.backing_up);
        run(files).await
    }

    /// A backup of this profile, written where the user says (M13). Off the
    /// async runtime: the dialog waits for the user, and Argon2 takes about
    /// half a second.
    async fn export_profile(&self, passphrase: String) -> Result<Response, EngineError> {
        let passphrase = zeroize::Zeroizing::new(passphrase);
        backup::check_passphrase(&passphrase)?;
        let store = store::Store::new(self.profile.data_dir(), self.secrets.clone());
        let suggested = format!("couch-coapp-{}.couchbackup", self.profile.name());
        self.backing_up(|files| async move {
            tokio::task::spawn_blocking(move || {
                let Some(path) = files.save_to(&suggested) else {
                    return Ok(Response::Backup { path: None });
                };
                let contents = store.backup_contents()?;
                let sealed = backup::seal(&contents, &passphrase)?;
                backup::write_file(&path, &sealed)?;
                tracing::info!(
                    rooms = contents.rooms.len(),
                    "wrote a backup of this profile"
                );
                Ok(Response::Backup {
                    path: Some(path.display().to_string()),
                })
            })
            .await
            .map_err(|_| EngineError::Stopped)?
        })
        .await
    }

    /// A backup the user picks, restored into this profile, which must have
    /// no rooms (M13). The room actor writes it, so no room is created or
    /// joined meanwhile; the app starts again afterwards to use it.
    async fn restore_profile(&self, passphrase: String) -> Result<Response, EngineError> {
        let passphrase = zeroize::Zeroizing::new(passphrase);
        let store = store::Store::new(self.profile.data_dir(), self.secrets.clone());
        // Before the dialog, to save the user picking a file for nothing;
        // the actor checks again.
        if store.has_rooms() {
            return Err(store::StoreError::HasRooms.into());
        }
        self.backing_up(|files| async move {
            let opened = tokio::task::spawn_blocking(move || {
                let Some(path) = files.open_from() else {
                    return Ok(None);
                };
                let contents = backup::open(&backup::read_file(&path)?, &passphrase)?;
                Ok::<_, EngineError>(Some((path, contents)))
            })
            .await
            .map_err(|_| EngineError::Stopped)??;
            let Some((path, contents)) = opened else {
                return Ok(Response::Backup { path: None });
            };
            // Held through the restore: no relay change may write over the
            // relays it brings, and none is taken until the app starts again
            // with them (M26a).
            let mut relays = self.relays.lock().await;
            let (reply, response) = oneshot::channel();
            self.actor
                .send(rooms::Input::Restore(Box::new(contents), reply))
                .await
                .map_err(|_| EngineError::Stopped)?;
            response.await.map_err(|_| EngineError::Stopped)??;
            relays.frozen = true;
            drop(relays);
            tracing::info!("restored a backup into this profile");
            Ok(Response::Backup {
                path: Some(path.display().to_string()),
            })
        })
        .await
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
    fn shortcut_views(&self, shortcuts: &Shortcuts) -> Response {
        // The host's drawing key only where the host draws over the game
        // (M27b, Windows): it is hooked nowhere else.
        let draws = self.backend.draws_over_game();
        Response::Shortcuts {
            shortcuts: shortcuts
                .rows()
                .iter()
                .filter(|b| draws || b.shortcut != couch_input::shortcuts::Shortcut::HostDrawing)
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
            drawing_keys: DrawingKeyView::all(),
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
        Ok(self.shortcut_views(&shortcuts))
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
                commit: build_config::commit().map(str::to_string),
                repository: Some(env!("CARGO_PKG_REPOSITORY"))
                    .filter(|r| !r.is_empty())
                    .map(str::to_string),
                protocol: String::from_utf8_lossy(couch_proto::ALPN).into_owned(),
                profile: self.profile.name().to_string(),
                data_dir: self.profile.data_dir().display().to_string(),
                platform: Platform::current(),
                capabilities: self.backend.capabilities(),
                home_pc: self.backend.capabilities().sessions && self.backend.home_pc(),
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
            Command::GetShortcuts => {
                Ok(self.shortcut_views(&store::shortcuts(self.profile.data_dir())))
            }
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
            Command::GetNetwork => Ok(self.network_view()),
            Command::CheckNetwork => {
                network::lock(&self.network).forget_checks();
                self.endpoint.network_change().await;
                let _ = self.poke.try_send(());
                Ok(self.network_view())
            }
            Command::AddRelay {
                url,
                token,
                personal,
                unchecked,
            } => {
                let token = token.and_then(couch_net::endpoint::RelayToken::new);
                self.add_relay(&url, token, personal, unchecked).await
            }
            Command::AddRelayCode { code, unchecked } => {
                let code = relays::code::RelayCode::parse(&code)?;
                self.add_relay(code.url.as_str(), code.token, code.personal, unchecked)
                    .await
            }
            Command::ReadRelayCode { code } => {
                let code = relays::code::RelayCode::parse(&code)?;
                let state = self.relays.lock().await;
                let held = state
                    .saved
                    .relays
                    .iter()
                    .find(|r| r.url == code.url.as_str());
                Ok(Response::RelayCode(RelayCodeView {
                    url: code.url.to_string(),
                    host: relay_host(code.url.as_str()),
                    has_code: code.token.is_some(),
                    personal: code.personal,
                    held: held.is_some(),
                    same_code: held.is_some_and(|r| {
                        r.token.as_deref() == code.token.as_ref().map(|t| t.reveal())
                    }),
                }))
            }
            Command::ShareRelay { url } => {
                let code = {
                    let state = self.relays.lock().await;
                    let url: couch_net::RelayUrl = url
                        .trim()
                        .parse()
                        .map_err(|_| EngineError::Refused("that relay isn't in your list"))?;
                    let Some(relay) = state.saved.relays.iter().find(|r| r.url == url.as_str())
                    else {
                        return Err(EngineError::Refused("that relay isn't in your list"));
                    };
                    if relay.personal {
                        return Err(EngineError::Refused(
                            "this relay's code is personal: it was given to you alone, so it isn't shared",
                        ));
                    }
                    relays::code::RelayCode {
                        url,
                        token: relay
                            .token
                            .clone()
                            .and_then(couch_net::endpoint::RelayToken::new),
                        personal: false,
                    }
                    .encode()
                };
                self.backend
                    .copy_secret(&code)
                    .map_err(|e| EngineError::Media(e.to_string()))?;
                Ok(Response::Done)
            }
            Command::RemoveRelay { url } => {
                self.change_relays(|state| {
                    let url: couch_net::RelayUrl = url
                        .trim()
                        .parse()
                        .map_err(|_| EngineError::Refused("that relay isn't in your list"))?;
                    state.saved.remove(&url)?;
                    tracing::info!(%url, "relay removed");
                    Ok(())
                })
                .await
            }
            Command::SetPublicRelays { on } => {
                self.change_relays(|state| {
                    state.saved.keep_public = on;
                    Ok(())
                })
                .await
            }
            Command::SetPublicFallback { on } => {
                if !on {
                    // Off now, not once the watch notices.
                    network::lock(&self.network).stop_falling_back();
                }
                self.change_relays(|state| {
                    state.saved.public_fallback = on;
                    Ok(())
                })
                .await
            }
            Command::DeferRelay { never } => {
                self.change_relays(|state| {
                    state.saved.ask = if never {
                        relays::AskForRelay::Never
                    } else {
                        relays::AskForRelay::Later {
                            until: remote::unix_now() + relays::ASK_LATER_SECS,
                            version: env!("CARGO_PKG_VERSION").to_string(),
                        }
                    };
                    Ok(())
                })
                .await
            }
            Command::GetDiagnostics => self.diagnostics().await,
            Command::OpenLogFolder => {
                let dir = self.profile.data_dir().to_path_buf();
                let backend = self.backend.clone();
                tokio::task::spawn_blocking(move || backend.open_folder(&dir))
                    .await
                    .map_err(|e| EngineError::Media(e.to_string()))?
                    .map_err(|e| EngineError::Media(e.to_string()))?;
                Ok(Response::Done)
            }
            Command::GetStreamWindow => Ok(Response::StreamWindow(StreamWindowSettingsView {
                bar: store::stream_window(self.profile.data_dir()).bar,
            })),
            Command::GetTray => Ok(Response::Tray(TrayView { on: self.tray_on() })),
            Command::GetDrawingDefault => {
                let drawing = store::drawing(self.profile.data_dir());
                Ok(Response::Drawing(DrawingSettingsView {
                    on: drawing.on,
                    pictures: drawing.pictures,
                }))
            }
            Command::SetDrawingDefault { on, pictures } => {
                store::save_drawing(
                    self.profile.data_dir(),
                    store::DrawingSettings { on, pictures },
                )?;
                Ok(Response::Drawing(DrawingSettingsView { on, pictures }))
            }
            Command::GetStreaming => Ok(Response::Streaming(StreamingSettingsView {
                hdr: store::streaming(self.profile.data_dir()).hdr,
            })),
            Command::SetStreaming { hdr } => {
                store::save_streaming(self.profile.data_dir(), store::StreamingSettings { hdr })?;
                Ok(Response::Streaming(StreamingSettingsView { hdr }))
            }
            Command::SetTray { on } => {
                store::save_app(self.profile.data_dir(), store::AppSettings { tray: on })?;
                self.tray.store(on, Ordering::SeqCst);
                Ok(Response::Tray(TrayView { on }))
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
                Ok(Response::Sources(SourceList {
                    windows: sources
                        .windows
                        .into_iter()
                        .map(|w| WindowView {
                            id: w.id.to_string(),
                            title: w.title,
                            app: w.app,
                            elevated: w.elevated,
                            refresh_hz: w.refresh_hz,
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
                            refresh_hz: s.refresh_hz,
                        })
                        .collect(),
                    controllers: u32::try_from(controllers).unwrap_or(u32::MAX),
                    host_controllers: host_controllers
                        .into_iter()
                        .map(|c| HostController {
                            name: c.name,
                            vendor: c.ids.vendor,
                            product: c.ids.product,
                            kind: match c.kind {
                                ControllerKind::Physical => HostControllerKind::Physical,
                                ControllerKind::OtherVirtual => HostControllerKind::OtherVirtual,
                            },
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
            Command::ListSoundSources => {
                if !self.backend.picks_sound() {
                    return Err(EngineError::Refused(
                        "this PC sends the streamed window's own sound",
                    ));
                }
                let backend = self.backend.clone();
                let programs = tokio::task::spawn_blocking(move || backend.sound_programs())
                    .await
                    .map_err(|_| EngineError::Stopped)?
                    .map_err(|e| EngineError::Media(e.to_string()))?;
                Ok(Response::SoundSources { programs })
            }
            Command::ShowNotices { mut notices } => {
                // Cut, never refused: a refusal would leave up what's
                // already answered. The first are the requests. Each id
                // once, and whole: two notices on one id would leave one
                // that can't be closed.
                let mut ids = std::collections::HashSet::new();
                notices.retain(|n| n.id.len() <= MAX_NOTICE_TEXT && ids.insert(n.id.clone()));
                notices.truncate(MAX_NOTICES);
                for n in &mut notices {
                    clip(&mut n.title, MAX_NOTICE_TEXT);
                    clip(&mut n.body, MAX_NOTICE_TEXT);
                }
                self.backend
                    .show_notices(notices)
                    .map_err(|e| EngineError::Media(e.to_string()))?;
                Ok(Response::Done)
            }
            Command::GetDrivers => self.drivers().await,
            Command::SharedChoice { forget } => {
                if forget {
                    self.backend.forget_shared_choice();
                }
                Ok(Response::SharedChoice {
                    remembered: self.backend.remembers_shared_choice(),
                })
            }
            Command::PlayChime => {
                self.backend.play_chime();
                Ok(Response::Done)
            }
            Command::FullScreenGame => Ok(Response::FullScreenGame {
                on: self.backend.exclusive_full_screen(),
            }),
            Command::ExportProfile { passphrase } => self.export_profile(passphrase).await,
            Command::RestoreProfile { passphrase } => self.restore_profile(passphrase).await,
            Command::GetNotices => {
                let path = std::env::current_exe()
                    .map_err(|e| EngineError::Notices(e.to_string()))?
                    .with_file_name(NOTICES_FILE);
                read_notices(&path).map(|text| Response::Notices { text })
            }
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
        // A request's notification would outlive the session (M16c).
        let backend = self.backend.clone();
        let _ = tokio::task::spawn_blocking(move || backend.close_notices()).await;
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

    /// For tests: dials `member` in `room` again while connected. When we
    /// dialed the connection up now, the room keeps the new one and closes
    /// the old one as a duplicate, on both sides: a connection replaced
    /// while still working (M16a).
    #[doc(hidden)]
    pub async fn dial_again(&self, room: &str, member: &str) {
        let _ = self
            .actor
            .send(rooms::Input::DialAgain(
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
            relays: Relays::Fixed(RelayChoice::Disabled),
            secret_store: Arc::new(secrets::Unprotected),
            ..EngineOptions::development()
        };
        Engine::start(Profile::new("test", dir).unwrap(), options)
            .await
            .unwrap()
    }

    #[test]
    fn notices_are_cut_at_a_characters_edge() {
        let mut text = "ação".to_string();
        clip(&mut text, 2);
        assert_eq!(text, "a");
        clip(&mut text, 10);
        assert_eq!(text, "a");
    }

    #[test]
    fn window_titles_stay_out_of_debug() {
        let shown = format!(
            "{:?}",
            Command::StartStream {
                source: SourceChoice::Window { id: "42".into() },
                name: "Bank of Somewhere - Chrome".into(),
                fps: 60,
                max_mbps: 20,
                upload_mbps: None,
                program: Some("Bank of Somewhere".into()),
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
                    refresh_hz: None,
                }],
                screens: Vec::new(),
                controllers: 0,
                host_controllers: Vec::new(),
                hiding: HidingView::NotInstalled,
                virtual_pads: true,
            })
        );
        assert!(!list.contains("Bank"), "{list}");
        // The host's view of its stream (M19), which `Response::Room` shows.
        let streaming = format!(
            "{:?}",
            StreamView {
                name: "Bank of Somewhere - Chrome".into(),
                fps: 60,
                max_mbps: 20,
                upload_mbps: None,
                screen: false,
            }
        );
        assert!(!streaming.contains("Bank"), "{streaming}");
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
            relays: Relays::Fixed(RelayChoice::Disabled),
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
        let Response::Shortcuts { shortcuts, .. } =
            engine.handle(Command::GetShortcuts).await.unwrap()
        else {
            panic!("expected shortcuts");
        };
        assert_eq!(shortcuts[0].keys, "Ctrl+Alt+Shift+Backspace");
        assert_eq!(shortcuts[0].scope, ShortcutScope::Host);
        assert!(shortcuts[0].is_default && shortcuts[0].chord);
        // The host's drawing key only where the host draws over the game
        // (M27b, Windows): it is hooked nowhere else.
        let drawing = shortcuts.iter().any(|s| s.id == "hostDrawing");
        assert_eq!(drawing, cfg!(windows));
        assert_eq!(
            shortcuts.len() + usize::from(!drawing),
            couch_input::shortcuts::SHORTCUTS.len()
        );
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
        let Response::Shortcuts { shortcuts, .. } = set("panic", "KeyP").await.unwrap() else {
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

    #[tokio::test]
    async fn relays_are_registered_replaced_and_removed_while_running() {
        let dir = tempfile::tempdir().unwrap();
        let options = EngineOptions {
            relays: Relays::Profile(RelayRules {
                built_in: None,
                test_build: true,
                public_relays: false,
            }),
            secret_store: Arc::new(secrets::Unprotected),
            network: NetworkTimings {
                check_timeout: Duration::from_secs(2),
                ..NetworkTimings::default()
            },
            ..EngineOptions::development()
        };
        let start = || async {
            Engine::start(Profile::new("test", dir.path()).unwrap(), options.clone())
                .await
                .unwrap()
        };
        let view = |response: Result<Response, EngineError>| match response.unwrap() {
            Response::Network(view) => view,
            other => panic!("expected the network view, got {other:?}"),
        };
        let add = |url: &str, token: Option<&str>| Command::AddRelay {
            url: url.into(),
            token: token.map(str::to_string),
            personal: false,
            unchecked: true,
        };

        let engine = start().await;
        let first = view(engine.handle(Command::GetNetwork).await);
        assert!(first.relays.is_empty() && first.changeable && first.test_build);
        assert!(!first.public_in_use, "tests offer no public relays");

        // Checked first: nothing there, so nothing added, and saying so.
        let gone = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let nowhere = format!("http://127.0.0.1:{gone}");
        let checked = engine
            .handle(Command::AddRelay {
                url: nowhere.clone(),
                token: Some("s3cret".into()),
                personal: false,
                unchecked: false,
            })
            .await
            .unwrap();
        let Response::RelayCheck(check) = checked else {
            panic!("expected the check's answer, got {checked:?}");
        };
        assert_eq!(check.host, "127.0.0.1");
        assert!(matches!(
            check.outcome,
            RelayCheckOutcome::Unreachable { .. }
        ));
        assert!(
            view(engine.handle(Command::GetNetwork).await)
                .relays
                .is_empty()
        );
        // A code that doesn't read says why, and adds nothing.
        let damaged = engine
            .handle(Command::AddRelayCode {
                code: "couch-relay-1.aGVsbG8.00000000".into(),
                unchecked: true,
            })
            .await
            .unwrap_err();
        assert!(damaged.to_string().contains("copy it again"), "{damaged}");
        assert_eq!(first.home_relay, None);

        for bad in [
            "relay.example.com",
            "http://relay.example.com",
            "https://",
            "https://user:token@relay.example.com",
        ] {
            assert!(engine.handle(add(bad, None)).await.is_err(), "{bad}");
        }
        let added = view(
            engine
                .handle(add(" http://localhost:3340 ", Some("s3cret")))
                .await,
        );
        let [relay] = added.relays.as_slice() else {
            panic!("one relay: {:?}", added.relays);
        };
        assert_eq!(relay.url, "http://localhost:3340/");
        assert_eq!(relay.host, "localhost");
        assert!(relay.has_code && !relay.built_in && !relay.personal);
        assert!(!format!("{added:?}").contains("s3cret"));
        assert!(!format!("{:?}", add("http://localhost:3340", Some("s3cret"))).contains("s3cret"));

        // A new code for the same relay replaces the old one.
        let replaced = view(engine.handle(add("http://localhost:3340", None)).await);
        assert_eq!(replaced.relays.len(), 1);
        assert!(!replaced.relays[0].has_code);
        let kept = view(engine.handle(Command::SetPublicRelays { on: true }).await);
        assert!(kept.keep_public);
        // Standing in when none of ours works: off by default, the user's
        // to allow (here never acting: these tests offer no public relays).
        assert!(!kept.public_fallback);
        // Putting the wizard off is saved with the relays.
        engine
            .handle(Command::DeferRelay { never: true })
            .await
            .unwrap();
        assert_eq!(
            engine.relays.lock().await.saved.ask,
            relays::AskForRelay::Never
        );
        let allowed = view(engine.handle(Command::SetPublicFallback { on: true }).await);
        assert!(allowed.public_fallback && !allowed.falling_back);
        engine.shutdown().await;
        drop(engine);

        // The next start holds them.
        let engine = start().await;
        let again = view(engine.handle(Command::GetNetwork).await);
        assert_eq!(again.relays, replaced.relays);
        assert!(again.keep_public);
        assert!(again.public_fallback, "kept");
        let removed = view(
            engine
                .handle(Command::RemoveRelay {
                    url: "http://localhost:3340".into(),
                })
                .await,
        );
        assert!(removed.relays.is_empty());
        assert!(
            engine
                .handle(Command::RemoveRelay {
                    url: "http://localhost:3340".into(),
                })
                .await
                .is_err()
        );
        engine.shutdown().await;
        drop(engine);

        // Relays fixed at start (tests) can't be changed.
        let fixed = super::tests::engine(dir.path()).await;
        assert!(!view(fixed.handle(Command::GetNetwork).await).changeable);
        assert!(
            fixed
                .handle(add("http://localhost:3340", None))
                .await
                .is_err()
        );
        fixed.shutdown().await;
    }

    #[test]
    fn the_notices_are_read_and_a_missing_file_says_how_to_make_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(NOTICES_FILE);
        let missing = read_notices(&path).unwrap_err().to_string();
        assert!(missing.contains("cargo xtask notices"), "{missing}");
        std::fs::write(&path, "Couch Co-App 0.9.0\nMIT License\n").unwrap();
        assert_eq!(
            read_notices(&path).unwrap(),
            "Couch Co-App 0.9.0\nMIT License\n"
        );
        // Licence files aren't always UTF-8; the rest still reads.
        std::fs::write(&path, b"Fran\xe7ois\n").unwrap();
        assert!(read_notices(&path).unwrap().starts_with("Fran"));
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
            founder: None,
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
            // The password a new owner chooses (M13).
            format!(
                "{:?}",
                Command::AnswerOffer {
                    room: "00".into(),
                    accept: true,
                    password: Some(secret.into())
                }
            ),
            format!(
                "{:?}",
                Command::PreviewInvite {
                    invite: secret.into()
                }
            ),
            // A remote-play PIN opens the home PC (M25), and a stream's name
            // sent to it is a window's title.
            format!("{:?}", Command::SetRemotePin { pin: secret.into() }),
            format!(
                "{:?}",
                Command::UnlockRemote {
                    room: "00".into(),
                    member: "11".into(),
                    pin: secret.into()
                }
            ),
            format!(
                "{:?}",
                Command::Remote {
                    room: "00".into(),
                    member: "11".into(),
                    action: RemoteAction::StartStream {
                        source: SourceChoice::Window { id: "7".into() },
                        name: secret.into(),
                        fps: 60,
                        max_mbps: 20,
                        upload_mbps: None,
                    }
                }
            ),
            // A backup's passphrase opens the whole identity (M13).
            format!(
                "{:?}",
                Command::ExportProfile {
                    passphrase: secret.into()
                }
            ),
            format!(
                "{:?}",
                Command::RestoreProfile {
                    passphrase: secret.into()
                }
            ),
            format!(
                "{:?}",
                Response::Backup {
                    path: Some(secret.into())
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
