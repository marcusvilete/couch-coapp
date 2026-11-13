//! The streaming pipelines, shared by the desktop app and `couch-cli`:
//! - [`host`] streams one session to its viewers;
//! - [`watch`] shows a session in `couch-viewer`.
//!
//! Both run over one iroh connection per pair, sharing that connection's
//! datagram reader and stream acceptor (`couch_net::datagrams`,
//! `couch_net::streams`) with whatever else uses it.
//!
//! What they need from the machine (capture, encoders, sound, injection, the
//! viewer process) comes through [`Backend`], so tests run them with fakes
//! and without a GPU.
//!
//! **Input never waits on anything else.** The host's input runs in a task
//! of its own, from the connection through the seat router to the
//! injectors. The room actor only sends it grants and revocations.

pub mod host;
pub mod ipc;
#[cfg(target_os = "linux")]
mod linux;
pub mod own_window;
#[cfg(windows)]
pub mod platform;
pub mod source;
mod viewer_process;
pub mod virtual_hiding;
pub mod watch;

use std::{
    any::Any,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use couch_input::{
    HidUsage, InputError, KbmInjector, Plugged,
    local::{ControllerIds, LocalPads},
    shortcuts::Shortcut,
};
use couch_media::{
    AudioSource, Capturer, EncoderConfig, GpuFrame, MediaError, VideoEncoder, clock::Clock,
};

use crate::{DriverName, DriverState, ScreenArea};
use tokio::io::{AsyncRead, AsyncWrite};

/// A native window or screen handle (an `HWND` or `HMONITOR` on Windows).
pub type NativeId = isize;

/// What a host streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A window, by its native handle, as [`Backend::sources`] listed it.
    Window(NativeId),
    /// A screen, by its native handle, as [`Backend::sources`] listed it.
    Screen(NativeId),
    /// The first window whose title contains this text (`couch-cli host`).
    WindowTitle(String),
    /// The primary screen (`couch-cli host --source monitor`).
    PrimaryScreen,
    /// Generated frames, for tests and measurements.
    TestPattern { width: u32, height: u32 },
}

impl Source {
    /// Keyboard and mouse can only be aimed at a window: they go in only
    /// while it is in front.
    pub fn is_window(&self) -> bool {
        matches!(self, Self::Window(_) | Self::WindowTitle(_))
    }
}

/// What a host could stream right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    pub windows: Vec<WindowSource>,
    pub screens: Vec<ScreenSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSource {
    pub id: NativeId,
    pub title: String,
    /// The program's file name, such as `Celeste.exe`; may be empty.
    pub app: String,
    /// It runs with more rights than we do (as administrator).
    pub elevated: bool,
    /// The refresh rate of the screen it is on, if known: a capture gets no
    /// more frames a second than that.
    pub refresh_hz: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSource {
    pub id: NativeId,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub refresh_hz: Option<u32>,
}

/// How to capture for a stream ([`Backend::capture`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureRequest {
    /// The stream's frame rate.
    pub fps: u32,
    /// Frames in memory: the capture can't keep them on the GPU, or no
    /// encoder in this stream reads them there and they aren't read back
    /// after pacing (M12a: unpaced, or `COUCH_HOST_MEMORY_PACING=0`).
    pub in_memory: bool,
    /// The stream paces frames itself (M9d, `couch_media::pacing`), so
    /// frames on the GPU may come faster than `fps`. Frames in memory never
    /// do: each costs a copy. (Since M12a a host whose encoders take frames
    /// in memory still captures on the GPU, paced, and reads back only the
    /// frames sent.)
    pub paced: bool,
    /// Nobody may be at the PC (a stream started from afar): a dialog the
    /// capture brings up is given up after [`UNANSWERED_AFTER`] (M16c).
    pub unattended: bool,
    /// A monitor in HDR is captured in FP16 and converted to SDR with its
    /// SDR white level (M29; Windows, unless `COUCH_HOST_HDR=0`). Off,
    /// Windows' own 8-bit capture, which clips HDR content at 80 nits.
    pub hdr: bool,
}

/// How long a capture started from afar waits for its dialog to be
/// answered at the PC (the screen-sharing portal's, M16c) before nothing
/// counts as shared.
pub const UNANSWERED_AFTER: Duration = Duration::from_secs(30);

/// Which sound a host streams (`couch-cli host --audio`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AudioChoice {
    /// The game for a window, everything but us for a screen, test clicks
    /// for the test pattern.
    #[default]
    Auto,
    /// The captured window's process and its children.
    Game,
    /// Everything this PC plays, except this app and its viewers.
    System,
    /// One program's sound, by its name ([`Backend::sound_programs`]).
    Program(String),
    /// The click track.
    Tone,
    Off,
}

/// A sound source, once the choice is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioKind {
    /// The process behind this window, and its children.
    Game(NativeId),
    System,
    /// One program's, by name, where the backend [picks
    /// sound](Backend::picks_sound).
    Program(String),
    Tone,
}

/// Whether the host's controller can be hidden from games
/// ([`Backend::hiding`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hiding {
    /// Nothing to hide with: HidHide isn't installed.
    Unavailable,
    Ready,
    /// HidHide stayed busy for a second: most likely its configuration
    /// window is open.
    Busy,
}

/// Where the device of a remapper's pad the host keeps hidden stands now
/// ([`Backend::virtual_device_now`], M22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualDevice {
    /// Plugged in, still another program's virtual pad.
    Theirs,
    /// Plugged in, and one of the hosts' own pads, by its serial.
    Ours,
    /// Plugged in, but whose can't be told now (a host plugging a pad, the
    /// markers unreadable). While plugged in, no other pad can have its id.
    InDoubt,
    /// Not plugged in; `hosts_plugging`: a host on this PC is plugging a
    /// pad, which may take its id.
    Gone { hosts_plugging: bool },
}

/// Where keyboard and mouse would land, checked right before every input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Focus {
    /// The game window is in front.
    pub foreground: bool,
    /// The pointer is over it.
    pub pointer_inside: bool,
}

/// How to show the viewer window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewerOptions {
    pub position: Option<(i32, i32)>,
    pub size: Option<(u32, u32)>,
    /// Start with the HUD hidden (it would cover a latency barcode).
    pub hide_hud: bool,
    /// Start without the session bar, the centre card and stats still
    /// showing (P11: Settings → Stream window).
    pub hide_bar: bool,
    /// Start with the stats panel closed (the app; the HUD has a button).
    pub hide_stats: bool,
    /// Write the viewer's log here, without a console window. `None` lets it
    /// log to ours (the CLI).
    pub log: Option<PathBuf>,
    /// The host's own window of its stream (M27d, `--own-stream`): no
    /// controllers, sound or input, left out of captures.
    pub own_stream: bool,
}

/// A running viewer: its stdin and stdout carry `couch_proto::ipc`.
pub struct ViewerProcess {
    pub stdin: Pin<Box<dyn AsyncWrite + Send>>,
    pub stdout: Pin<Box<dyn AsyncRead + Send>>,
    /// Keeps the process alive; dropping it ends a real viewer.
    pub guard: Box<dyn Any + Send>,
    /// Its process id, to let it come to the front (P11); `None` for fakes.
    pub pid: Option<u32>,
}

/// The game behind a streamed window ([`Backend::game_process`]), watched so
/// the host can follow it to a new window or tell that it exited
/// ([`source`]).
pub trait GameProcess: Send + Sync {
    fn running(&self) -> bool;
    /// Its windows that could replace the captured one, best first.
    fn windows(&self) -> Vec<NativeId>;
}

/// What the pipelines need from the machine.
pub trait Backend: Send + Sync + 'static {
    /// The clock frames and packets are stamped with (QPC on Windows), which
    /// the viewer process shares.
    fn clock(&self) -> Arc<dyn Clock>;

    /// What could be streamed right now.
    fn sources(&self) -> Result<Sources, MediaError>;

    /// Starts capturing for a stream at `request.fps`. Also returns the
    /// captured window, if the source is one. Frames may come in memory or
    /// on the GPU, unless `request.in_memory`.
    fn capture(
        &self,
        source: &Source,
        request: CaptureRequest,
    ) -> Result<(Box<dyn Capturer>, Option<NativeId>), MediaError>;

    /// An encoder. With `gpu` (a frame like the ones to come), one that reads
    /// GPU frames where it can (`caps().gpu_input`); otherwise one that takes
    /// frames in memory, which GPU frames are copied to.
    fn encoder(
        &self,
        config: EncoderConfig,
        gpu: Option<&GpuFrame>,
    ) -> Result<Box<dyn VideoEncoder>, MediaError>;

    /// The sound `kind` names. `stop` is the stream's: a source that waits
    /// before it starts (Linux's, for the last capture to end) gives up,
    /// `MediaError::Closed`, once it is set.
    fn audio(&self, kind: AudioKind, stop: &AtomicBool)
    -> Result<Box<dyn AudioSource>, MediaError>;

    /// Plugs in a virtual pad. Blocks for a few seconds at most: a pad that
    /// isn't taking input by then comes back pending, Windows still setting
    /// its device up (P10). `real_ids` gives it a real Xbox 360 controller's
    /// ids instead of the development ids a viewer on this machine skips.
    fn plug_pad(&self, real_ids: bool) -> Result<Plugged, InputError>;

    fn kbm(&self) -> Box<dyn KbmInjector>;

    /// Game controllers plugged into this PC, the hosts' own virtual pads
    /// left out (P9); other programs' count, since games see them. Go live
    /// warns about them in multiplayer: a host's controller can take a
    /// player's place in the game.
    fn controllers(&self) -> usize {
        0
    }

    /// The host's own controllers, which can play through a seat. Never a
    /// virtual pad.
    fn local_pads(&self) -> Option<&dyn LocalPads> {
        None
    }

    /// Whether a controller could be hidden from games now.
    fn hiding(&self) -> Hiding {
        Hiding::Unavailable
    }

    /// Hides the host's controller from every program but this one until
    /// the returned guard drops (HidHide on Windows). Blocks briefly.
    fn hide_controller(&self, _ids: ControllerIds) -> Result<Box<dyn Any + Send>, String> {
        Err("hiding controllers isn't available here".into())
    }

    /// Hides another program's virtual pad, the device `device` (its
    /// instance id), from every program but this one until the returned
    /// guard drops (M22), the pad gone or not: never ours, nor one in doubt.
    /// Blocks briefly.
    fn hide_virtual(&self, _device: &str) -> Result<Box<dyn Any + Send>, String> {
        Err("hiding controllers isn't available here".into())
    }

    /// Where a remapper pad's device kept hidden stands now (M22).
    fn virtual_device_now(&self, _device: &str) -> VirtualDevice {
        VirtualDevice::InDoubt
    }

    /// Tunes this PC's network for streaming until the returned guard drops
    /// (Wi-Fi media streaming mode on Windows, M9a). `None` when there is
    /// nothing to tune; never an error, since a stream runs fine without it.
    fn low_latency_network(&self) -> Option<Box<dyn Any + Send>> {
        None
    }

    fn focus(&self, window: NativeId) -> Focus;

    fn window_exists(&self, window: NativeId) -> bool;

    /// The process behind `window`, to watch while it is streamed. `None`
    /// when there is nothing to watch: a lost window then ends the stream at
    /// once.
    fn game_process(&self, _window: NativeId) -> Option<Box<dyn GameProcess>> {
        None
    }

    fn launch_viewer(&self, options: &ViewerOptions) -> Result<ViewerProcess, MediaError>;

    /// Whether this PC can be a home PC that one of the user's devices runs
    /// from afar (M25): kept awake, its lock screen read, and a stream
    /// started without anyone at it. Windows, and Linux since M16c-1's
    /// fifth part.
    fn home_pc(&self) -> bool {
        false
    }

    /// Whether a stream captures from its start, watched or not, and goes on
    /// while nobody watches. Elsewhere capture runs only while someone
    /// watches; on Linux starting a capture may ask the user in the
    /// desktop's dialog (M16c), which must come when the host starts the
    /// stream, not over the game when a guest arrives.
    fn captures_without_viewers(&self) -> bool {
        false
    }

    /// Whether a capture of `source` can start with nobody at the PC: on
    /// Linux only a remembered choice skips the screen-sharing dialog
    /// (M16c), so a stream started from afar needs one.
    fn shares_unattended(&self, _source: &Source) -> bool {
        true
    }

    /// Whether a stream shares again, without asking, what was last chosen
    /// in the desktop's screen-sharing dialog (Linux, M16c).
    fn remembers_shared_choice(&self) -> bool {
        false
    }

    /// Forgets that choice: the next stream asks in the dialog again.
    fn forget_shared_choice(&self) {}

    /// Puts `text` on the clipboard as a secret (M26a: a relay code, which
    /// holds its token): kept out of the clipboard's history and its sync
    /// to other devices where the system has them (Windows; KDE's history).
    /// Done here rather than in the webview, which never sees a token.
    fn copy_secret(&self, _text: &str) -> Result<(), MediaError> {
        Err(MediaError::Unsupported(
            "the clipboard on this system".into(),
        ))
    }

    /// Opens a profile's folder (its logs) in the system's file manager
    /// (M26a, Settings → Network). The webview never names the path.
    fn open_folder(&self, _dir: &std::path::Path) -> Result<(), MediaError> {
        Err(MediaError::Unsupported(
            "opening folders on this system".into(),
        ))
    }

    /// Whether a game in exclusive full screen is in front (Windows): no
    /// window of ours may come up over it, or it leaves full screen.
    fn exclusive_full_screen(&self) -> bool {
        false
    }

    /// Plays the request chime, returning at once, where the webview can't
    /// (Linux); this process's own sound, so out of the stream.
    fn play_chime(&self) {}

    /// Whether the host picks its stream's sound by program (Linux, M16c:
    /// the portal doesn't say whose the shared window is), from
    /// [`Backend::sound_programs`]. Elsewhere a window's own sound follows
    /// it.
    fn picks_sound(&self) -> bool {
        false
    }

    /// The programs playing sound now, by name, this app left out. Blocks
    /// briefly.
    fn sound_programs(&self) -> Result<Vec<String>, MediaError> {
        Err(MediaError::Unsupported(
            "picking a program's sound on this platform".into(),
        ))
    }

    /// Makes `notices` the host's desktop notifications up now, where they
    /// stand in for the overlay (Linux, M16c). Returns at once.
    fn show_notices(&self, _notices: Vec<crate::NoticeView>) -> Result<(), MediaError> {
        Err(MediaError::Unsupported(
            "notifications: this PC has the overlay".into(),
        ))
    }

    /// Closes the notices up, at the engine's end: none is left for a
    /// session that has gone. Blocks briefly.
    fn close_notices(&self) {}

    /// Lets the viewer process `pid` bring its window to the front next
    /// (P11: the app's Show window). Windows refuses a background process
    /// otherwise; the app is in front when its button is clicked.
    fn allow_foreground(&self, _pid: u32) {}

    /// Brings the streamed `window` to the front (M25b), restoring it if it
    /// is minimized; `false` if Windows wouldn't.
    fn bring_to_front(&self, _window: NativeId) -> bool {
        false
    }

    /// The whole of a streamed screen, taskbar included, where desktop
    /// control keeps the pointer (M25b); `None` for a window, or a screen
    /// that is gone (moves then stay relative).
    fn screen_area(&self, _source: &Source) -> Option<ScreenArea> {
        None
    }

    /// Keeps the system awake, and with `display` the screen on too, until
    /// the guard drops (M25c). `None` where it can't.
    fn keep_awake(&self, _display: bool) -> Option<Box<dyn Any + Send>> {
        None
    }

    /// Whether the lock screen (on Windows, or a UAC prompt) shows rather
    /// than the user's desktop (M25c). Returns at once.
    fn screen_locked(&self) -> bool {
        false
    }

    /// Whether this app starts when the user signs in (M25c).
    fn start_at_sign_in(&self) -> Result<bool, String> {
        Ok(false)
    }

    /// Starts this app at sign-in, to the notification area, or stops.
    fn set_start_at_sign_in(&self, _on: bool) -> Result<(), String> {
        Err("not on this system".into())
    }

    /// At start, where this app may start at sign-in: keeps a start at
    /// sign-in the user turned on pointing at this app (Linux: a newer
    /// AppImage replacing the one named, M16c).
    fn keep_start_at_sign_in(&self) {}

    /// At start, before any input: lets go of keys and buttons a host on
    /// this machine left held when it crashed or was killed, shows games any
    /// controller it left hidden, and makes a panic in this process let go
    /// of what it holds. Returns how many keys and buttons were left held.
    fn release_left_input(&self) -> usize {
        0
    }

    /// Each driver's state (Settings → Drivers, Go live's warnings), and
    /// whether its installer ships with this build. Elsewhere: none.
    fn drivers(&self) -> Vec<(DriverName, DriverState, bool)> {
        Vec::new()
    }

    /// Whether virtual controllers can be plugged in (ViGEmBus answers).
    fn virtual_pads(&self) -> bool {
        false
    }

    /// Runs `driver`'s shipped installer with Windows' administrator
    /// prompt and waits for it. Blocks: call it off the async runtime.
    fn install_driver(&self, _driver: DriverName) -> Result<(), String> {
        Err("drivers can't be installed on this platform".into())
    }

    /// Where the host's overlay goes for `source`: the work area of the
    /// monitor showing the window, or of the screen; the primary monitor's
    /// otherwise. `None` where there is no desktop (tests, other platforms).
    fn overlay_area(&self, _source: &Source) -> Option<ScreenArea> {
        None
    }

    /// Whether the host sees and draws on the board over the game here
    /// (M27b, ADR 0013): a board overlay of ours can lie over the captured
    /// window and take the focus for drawing. Windows; nowhere else yet.
    fn draws_over_game(&self) -> bool {
        false
    }

    /// Exactly what a capture of `source` shows, on the desktop, in
    /// physical pixels (M27b): a window's visible frame, a whole screen.
    /// `None` if it is gone or minimized: the board then waits off screen.
    fn board_area(&self, _source: &Source) -> Option<ScreenArea> {
        None
    }

    /// Lays the board overlay's window over `area` without activating it,
    /// or parks it off every screen (`None`).
    fn place_board(&self, _board: NativeId, _area: Option<ScreenArea>) {}

    /// Lets the board overlay take clicks and the focus (drawing), or makes
    /// it click-through and never activated again. `false` if it is gone.
    fn board_interactive(&self, _board: NativeId, _on: bool) -> bool {
        false
    }

    /// The window in front now, if known (M27b: given the front back when
    /// drawing over a streamed screen ends).
    fn foreground_window(&self) -> Option<NativeId> {
        None
    }

    /// The process whose window is in front, if known (M27d: the host's
    /// drawing key acts on the host's own stream window when it is).
    fn foreground_process(&self) -> Option<u32> {
        None
    }

    /// The window of process `pid`, once it shows one (M27d: the host's own
    /// stream window, which the engine brings forward itself: from a
    /// process not in front, the window's own raise is refused).
    fn window_of_process(&self, _pid: u32) -> Option<NativeId> {
        None
    }

    /// Where the host's own stream window opens (M27d): the work area of a
    /// monitor other than the one `source` is on, if there is one, so it
    /// doesn't cover the game. `None` lets it open where it likes.
    fn own_window_area(&self, _source: Option<&Source>) -> Option<ScreenArea> {
        None
    }

    /// Whether the host's own stream window is left out of what is
    /// captured (M27d: Windows' display affinity). A Linux share can't
    /// leave a window out: the console says to keep it off the screen
    /// shared.
    fn own_window_left_out(&self) -> bool {
        false
    }

    /// What this system can host (M16c, ADR 0012): sessions (capture,
    /// encode, guests' controllers), a keyboard-and-mouse seat, desktop
    /// control. Windows has them all; Linux sessions with controller seats
    /// (M16c-1) and desktop control (M16c-2's first part); elsewhere nothing.
    /// What's missing is refused before anything is set up or announced, and
    /// with `sessions` the home PC's side of remote play.
    fn capabilities(&self) -> crate::HostCapabilities {
        crate::HostCapabilities::default()
    }

    /// The codecs this PC decodes in hardware, as `couch_proto::codec` bits,
    /// for watching (M12c): H.264 always, HEVC where the GPU has its decoder.
    fn decodes(&self) -> u8 {
        couch_media::Codec::H264.bit()
    }

    /// Hooks the host's keys, Ctrl+Alt+Shift plus each key (the host rows
    /// of the user's `couch_input::shortcuts::Shortcuts`: the panic key,
    /// Accept and Decline), until the returned guard drops. On Windows only
    /// physical presses count, never injected ones; Linux's portal can't
    /// tell them apart ([`Backend::host_keys_hear_injected`], ADR 0012).
    /// `on_event` runs on the hook's own thread and must return at once.
    /// `unattended` (a session started from afar): a dialog binding them
    /// brings up is given up after [`UNANSWERED_AFTER`], the keys lost.
    fn host_keys(
        &self,
        keys: &[(HidUsage, Shortcut)],
        unattended: bool,
        on_event: Box<dyn Fn(HostKeyEvent) + Send>,
    ) -> Result<Box<dyn Any + Send>, MediaError>;

    /// Whether the host's keys also fire for keys injected into this PC
    /// (Linux's portal: a `uinput` key looks real to the desktop). Then
    /// Accept and Decline answer nothing while anyone but the host holds
    /// the keyboard seat, and guests can't press the host's chords (M16c-2,
    /// ADR 0012). Windows' hook ignores injected keys.
    fn host_keys_hear_injected(&self) -> bool {
        false
    }

    /// Whether the host names its game among the desktop's windows (Linux,
    /// M16c-2): its one source is a screen-sharing dialog's choice, which
    /// never says which window it is. A session there may give guests the
    /// keyboard seat on that source; their keys reach nothing until the host
    /// names the game ([`host::HostPipeline::set_game_window`]), then only while
    /// it is in front ([`Backend::focus`]).
    fn names_game_window(&self) -> bool {
        false
    }

    /// Starts the desktop's helper that tells which window is in front, for
    /// a session with a keyboard seat, until the guard drops (Linux,
    /// M16c-2). Returns at once; `None` where there is none to start.
    fn watch_front(&self) -> Option<Box<dyn Any + Send>> {
        None
    }

    /// The windows the host could name as its game (Linux, M16c-2), each
    /// with whether it has the shared picture's size. Empty until the
    /// helper has spoken.
    fn game_windows(&self) -> Vec<GameWindow> {
        Vec::new()
    }

    /// The host's desktop opens a full-screen view from a screen corner that
    /// takes guests' keys without another window coming to the front, which
    /// the helper can't see (KDE's Overview, M16c-2): the console asks the
    /// host to turn the corner off before giving a guest the keyboard.
    fn warns_hot_corner(&self) -> bool {
        false
    }
}

/// A window the host could name as its game on a shared screen
/// ([`Backend::game_windows`]). Its `Debug` leaves out the title.
#[derive(Clone, PartialEq, Eq)]
pub struct GameWindow {
    pub id: NativeId,
    pub title: String,
    /// Its program.
    pub app: String,
    /// It has the size of what is shared: likely the game.
    pub matches: bool,
}

impl std::fmt::Debug for GameWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameWindow")
            .field("id", &self.id)
            .field("title", &"..")
            .field("app", &self.app)
            .field("matches", &self.matches)
            .finish()
    }
}

/// What the host's keys report (`Backend::host_keys`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyEvent {
    /// One was pressed.
    Pressed(Shortcut),
    /// The desktop bound them after the hook was made, each with its keys as
    /// the desktop names them (Linux's portal, which may ask the user and
    /// let them change the keys; again whenever they change). A host key
    /// missing here isn't in force. Windows' hook is in force at once and
    /// never sends this.
    Bound(Vec<(Shortcut, String)>),
    /// They don't work, or stopped working (the desktop refused them, or
    /// its portal went away): the console says so.
    Lost(String),
}

/// This machine's backend: Windows' own, Linux's (M16c), or elsewhere one that
/// watches but can't host (M16b): it starts the stream window and refuses
/// everything hosting needs.
pub fn platform_backend() -> Arc<dyn Backend> {
    #[cfg(windows)]
    {
        // Whether this GPU decodes HEVC (M12c) takes a D3D11 device: asked
        // now, on a thread of its own, so the first `SessionJoin` (built in
        // the room actor) finds the answer cached.
        std::thread::spawn(couch_platform_win::video_caps::hevc_decode);
        Arc::new(platform::WindowsBackend)
    }
    #[cfg(target_os = "linux")]
    {
        Arc::new(linux::LinuxBackend::new(None))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Arc::new(ViewerOnly)
    }
}

/// [`platform_backend`] for the app's profile in `dir`: on Linux it keeps
/// the screen-sharing portal's last choice there (`portal.json`), so the
/// next run shares it again without the dialog.
pub fn profile_backend(dir: &std::path::Path) -> Arc<dyn Backend> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(linux::LinuxBackend::new(Some(dir.to_path_buf())))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        platform_backend()
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
struct ViewerOnly;

#[cfg(not(any(windows, target_os = "linux")))]
impl Backend for ViewerOnly {
    /// The stream window's clock (`couch-viewer`'s `now_us`), so times from
    /// it compare with ours.
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(couch_media::clock::ProcessClock::new())
    }
    fn sources(&self) -> Result<Sources, MediaError> {
        Ok(Sources::default())
    }
    fn capture(
        &self,
        _: &Source,
        _: CaptureRequest,
    ) -> Result<(Box<dyn Capturer>, Option<NativeId>), MediaError> {
        Err(unsupported())
    }
    fn encoder(
        &self,
        _: EncoderConfig,
        _: Option<&GpuFrame>,
    ) -> Result<Box<dyn VideoEncoder>, MediaError> {
        Err(unsupported())
    }
    fn audio(&self, _: AudioKind, _: &AtomicBool) -> Result<Box<dyn AudioSource>, MediaError> {
        Err(unsupported())
    }
    fn plug_pad(&self, _: bool) -> Result<Plugged, InputError> {
        Err(InputError::DriverUnavailable("not on this platform".into()))
    }
    fn kbm(&self) -> Box<dyn KbmInjector> {
        Box::new(NoKbm)
    }
    fn focus(&self, _: NativeId) -> Focus {
        Focus::default()
    }
    fn window_exists(&self, _: NativeId) -> bool {
        false
    }
    fn launch_viewer(&self, options: &ViewerOptions) -> Result<ViewerProcess, MediaError> {
        viewer_process::launch(options)
    }
    fn host_keys(
        &self,
        _: &[(HidUsage, Shortcut)],
        _: bool,
        _: Box<dyn Fn(HostKeyEvent) + Send>,
    ) -> Result<Box<dyn Any + Send>, MediaError> {
        Err(unsupported())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn unsupported() -> MediaError {
    MediaError::Unsupported("streaming on this platform".into())
}

/// Keyboard and mouse that go nowhere: a session's until its first grant of
/// either (nothing is made before: on Linux, no keyboard appears to the
/// desktop), or a system without them.
pub(super) struct NoKbm;

impl KbmInjector for NoKbm {
    fn key(&mut self, _: couch_input::HidUsage, _: bool) -> Result<(), InputError> {
        Ok(())
    }
    fn mouse_move(&mut self, _: i32, _: i32) -> Result<(), InputError> {
        Ok(())
    }
    fn mouse_button(&mut self, _: couch_input::MouseButton, _: bool) -> Result<(), InputError> {
        Ok(())
    }
    fn wheel(&mut self, _: i32, _: i32) -> Result<(), InputError> {
        Ok(())
    }
}
