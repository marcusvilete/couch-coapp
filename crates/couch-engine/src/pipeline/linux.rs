//! The Linux backend (M16c, ADR 0012): it watches (M16b), and hosts sessions
//! with controller seats (M16c-1): FFmpeg's Linux encoders (VAAPI, NVENC,
//! software); a window or screen the user shares through the desktop's
//! screen-sharing portal, and what the PC plays from PipeWire; guests'
//! virtual controllers through `uinput` (with its access set up from
//! Settings → Drivers); and the host's keys through the global-shortcuts
//! portal. Desktop control for a device of the user's, through a `uinput`
//! keyboard and mouse (M16c-2's first part), and guests' keyboard seat
//! while the desktop's helper tells which window is in front, the host naming
//! the game (its second part: `couch_platform_linux::front`).

use std::{
    any::Any,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError, atomic::AtomicBool},
    time::Duration,
};

use couch_codec_ffmpeg::FfmpegEncoder;
use couch_input::{
    DEV_PAD_IDS, HidUsage, InputError, KbmInjector, Plugged, VirtualPadFactory, XBOX_360_IDS,
    shortcuts::Shortcut,
};
use couch_media::{
    AudioSource, Capturer, EncoderConfig, GpuFrame, MediaError, VideoEncoder, audio::ToneSource,
    clock::Clock, testpattern::TestPattern,
};
use couch_platform_linux::{
    autostart, awake,
    clock::MonotonicClock,
    front::{self, gnome::ExtensionState},
    global_keys,
    kbm::UinputKbm,
    notices, pads, screencast, session_lock,
    sound::{Sounds, SystemSound},
    x11,
};

use super::{
    AudioKind, Backend, CaptureRequest, Focus, GameProcess, GameWindow, HostKeyEvent, NativeId,
    NoKbm, ScreenSource, Source, Sources, ViewerOptions, ViewerProcess, WindowSource,
    viewer_process,
};
use crate::{DriverName, DriverState, HostCapabilities, NoticeView, ScreenArea};

/// The one source a Linux host lists: whatever the user shares through the
/// desktop's dialog, a window or a screen. A screen to the pipeline (no
/// window to keep in front, the PC's sound).
pub const SHARED: NativeId = 1;

/// Why a Linux app may not start at sign-in.
const NOT_AN_APPIMAGE: &str = "only the AppImage starts at sign-in";

/// The profile's file for the screen-sharing portal's restore token.
const PORTAL_FILE: &str = "portal.json";

pub(super) struct LinuxBackend {
    /// Where the portal's restore token is kept (the app's profile), so a
    /// choice is remembered across runs; `None` keeps it for this run only
    /// (`couch-cli`).
    portal_file: Option<PathBuf>,
    /// The token for the last choice. Each is good for one start, and each
    /// start gives the next.
    restore_token: Arc<Mutex<Option<String>>>,
    /// The profile's folder (the KWin script goes there) and name; `None`
    /// for `couch-cli`.
    profile: Option<PathBuf>,
    /// What the current stream shares, once the user has chosen (M16c-2):
    /// its size picks out the game among the windows.
    shared: Arc<Mutex<Option<screencast::Shared>>>,
    /// On an X11 desktop (M28): the screens as last listed, for desktop
    /// control's pointer, read without asking the X server.
    screens: Mutex<Vec<x11::Screen>>,
}

impl LinuxBackend {
    pub(super) fn new(profile: Option<PathBuf>) -> Self {
        // The app's (not `couch-cli`'s): its menu entry, which GNOME finds
        // the window's icon by and the portals the app id (an AppImage's
        // only; the portals' first request installs it too).
        if profile.is_some() {
            match couch_platform_linux::launcher::ensure() {
                Ok(true) => tracing::info!("installed the menu entry for this AppImage"),
                Ok(false) => {}
                Err(e) => tracing::warn!("the menu entry couldn't be installed: {e}"),
            }
        }
        let portal_file = profile.as_ref().map(|dir| dir.join(PORTAL_FILE));
        let restore_token = portal_file.as_deref().and_then(|file| {
            let bytes = std::fs::read(file).ok()?;
            let saved: PortalFile = serde_json::from_slice(&bytes).ok()?;
            saved.restore_token
        });
        Self {
            portal_file,
            restore_token: Arc::new(Mutex::new(restore_token)),
            profile,
            shared: Arc::default(),
            screens: Mutex::default(),
        }
    }

    /// The X server's screens, kept for [`Backend::screen_area`].
    fn x11_screens(&self) -> Result<Vec<x11::Screen>, MediaError> {
        let screens = x11::screens().map_err(MediaError::Backend)?;
        *self.screens.lock().unwrap_or_else(PoisonError::into_inner) = screens.clone();
        Ok(screens)
    }

    /// A capture from the X server (M28): what the source names, listed by
    /// [`Backend::sources`], with no dialog.
    fn x11_capture(
        &self,
        source: &Source,
        fps: u32,
    ) -> Result<(Box<dyn Capturer>, Option<NativeId>), MediaError> {
        let id = |id: NativeId| {
            u32::try_from(id).map_err(|_| MediaError::Unsupported("that window or screen".into()))
        };
        let (target, window) = match source {
            Source::Window(window) => (x11::capture::Target::Window(id(*window)?), Some(*window)),
            Source::WindowTitle(text) => {
                let found = x11::windows()
                    .map_err(MediaError::Backend)?
                    .into_iter()
                    .find(|w| w.title.contains(text.as_str()))
                    .ok_or_else(|| {
                        MediaError::Backend(format!("no window's title has {text:?}"))
                    })?;
                (
                    x11::capture::Target::Window(found.id),
                    Some(native(found.id)),
                )
            }
            Source::Screen(screen) => {
                // Fresh for desktop control's pointer.
                if let Err(e) = self.x11_screens() {
                    tracing::warn!("the screens couldn't be listed: {e}");
                }
                (x11::capture::Target::Screen(id(*screen)?), None)
            }
            Source::PrimaryScreen => {
                let primary = self
                    .x11_screens()?
                    .into_iter()
                    .find(|s| s.primary)
                    .ok_or_else(|| MediaError::Backend("the X server lists no screen".into()))?;
                (x11::capture::Target::Screen(primary.id), None)
            }
            Source::TestPattern { .. } => unreachable!("the test pattern is made here"),
        };
        let capture = x11::capture::start(target, fps).map_err(MediaError::Backend)?;
        Ok((Box::new(capture), window))
    }

    /// A capture of what the user shares: the dialog, or the last choice
    /// again. `unattended`, a dialog nobody answers is given up.
    fn share(&self, fps: u32, unattended: bool) -> Result<Box<dyn Capturer>, MediaError> {
        let restore_token = self
            .restore_token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        // Spent by this start, in the file too: a start that fails before
        // the next one comes leaves nothing remembered, rather than a token
        // that would let a start from afar bring up the dialog again after
        // a restart.
        if restore_token.is_some() {
            save_token(self.portal_file.as_deref(), None);
        }
        // Nothing known of this stream's choice yet.
        *self.shared.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let shared = self.shared.clone();
        let on_shared = Box::new(move |what: screencast::Shared| {
            *shared.lock().unwrap_or_else(PoisonError::into_inner) = Some(what);
        });
        let (kept, file) = (self.restore_token.clone(), self.portal_file.clone());
        let on_token = Box::new(move |token: String| {
            save_token(file.as_deref(), Some(&token));
            *kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(token);
        });
        let capture = screencast::start(screencast::Options {
            fps,
            restore_token,
            on_token,
            on_shared,
            answer_within: unattended.then_some(super::UNANSWERED_AFTER),
        })
        .map_err(MediaError::Backend)?;
        Ok(Box::new(capture))
    }
}

/// An X11 window's or screen's id as the pipeline's: every id an X server
/// gives fits (29 bits), so `0`, which names none, is never one.
fn native(id: u32) -> NativeId {
    NativeId::try_from(id).unwrap_or(0)
}

/// Keeps `token` in `portal.json` (or none), where there is a file.
fn save_token(file: Option<&std::path::Path>, token: Option<&str>) {
    let Some(file) = file else { return };
    let saved = PortalFile {
        restore_token: token.map(str::to_owned),
    };
    let bytes = serde_json::to_vec(&saved).expect("a token serializes");
    if let Err(e) = crate::store::write_atomic(file, &bytes) {
        tracing::warn!("the shared window or screen won't be remembered: {e}");
    }
}

/// `portal.json`. This PC's own: never backed up (`store::BACKED_UP_FILES`
/// doesn't name it), nor logged.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortalFile {
    #[serde(default)]
    restore_token: Option<String>,
}

impl Backend for LinuxBackend {
    /// The stream window's clock (`couch-viewer`'s `now_us`), so times from
    /// it compare with ours.
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(MonotonicClock)
    }

    /// Sessions with controller seats (M16c-1); desktop control for a device
    /// of the user's once this user may make input devices (M16c-2), read
    /// each time (Set up may come while the app runs); and a keyboard seat
    /// for guests while the desktop also has a helper that tells which window
    /// is in front (KDE's KWin, or our GNOME extension on: `front`), as
    /// last looked. On an X11 desktop (M28-2) the X server itself says what
    /// is in front, keyboard grabs included (`x11::tracker`), on any desktop.
    fn capabilities(&self) -> HostCapabilities {
        let devices = pads::available();
        HostCapabilities {
            sessions: true,
            keyboard_seat: devices && (x11::session() || front::available().is_some()),
            desktop_control: devices,
        }
    }

    /// The portal never says which window it shares: the host names the
    /// game. On an X11 desktop the captured window is the game.
    fn names_game_window(&self) -> bool {
        !x11::session()
    }

    /// The KWin script loaded, or our GNOME extension asked to watch; on an
    /// X11 desktop neither (the X server says what is in front).
    fn watch_front(&self) -> Option<Box<dyn Any + Send>> {
        if x11::session() {
            return None;
        }
        let (dir, name) = match &self.profile {
            Some(dir) => (
                dir.clone(),
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            // `couch-cli`: a folder of this user's alone, never a shared
            // temporary one.
            None => match front::private_dir() {
                Some(dir) => (dir, format!("cli-{}", std::process::id())),
                None => {
                    tracing::warn!(
                        "what is in front can't be known: XDG_RUNTIME_DIR isn't set for the KWin script"
                    );
                    return None;
                }
            },
        };
        match front::watch(&dir, &name) {
            Ok(watch) => Some(Box::new(watch)),
            Err(e) => {
                tracing::warn!("what is in front can't be known: {e}");
                None
            }
        }
    }

    /// The helper's windows, this app's own left out, each marked if it has
    /// the shared picture's size.
    fn game_windows(&self) -> Vec<GameWindow> {
        if x11::session() {
            return Vec::new();
        }
        let size = self
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .and_then(|s| s.size);
        let ours = std::process::id();
        front::windows()
            .into_iter()
            .filter(|w| w.pid != ours)
            .map(|w| GameWindow {
                matches: size.is_some_and(|size| w.sizes.contains(&size)),
                id: w.id,
                title: w.title,
                app: w.app,
            })
            .collect()
    }

    /// KDE's top-left corner opens its Overview, whose search takes the keys
    /// while KWin keeps the game active (M16c-2's second review; the
    /// author's choice, 2026-11-04: the host is told). GNOME's overview the
    /// extension sees.
    fn warns_hot_corner(&self) -> bool {
        !x11::session() && front::available() == Some(front::Helper::KWin)
    }

    /// The desktop's global shortcuts can't tell a key a guest injects
    /// through `uinput` from the host's own (ADR 0012).
    fn host_keys_hear_injected(&self) -> bool {
        true
    }

    /// Kept awake through logind and the inhibit portal, its lock screen
    /// read from logind, and a stream from afar only with a remembered
    /// choice (M16c-1's fifth part).
    fn home_pc(&self) -> bool {
        true
    }

    /// One source: what the user shares in the desktop's dialog when the
    /// stream starts. On an X11 desktop (M28) the X server's windows and
    /// screens, as on Windows.
    fn sources(&self) -> Result<Sources, MediaError> {
        if x11::session() {
            let windows = x11::windows()
                .map_err(MediaError::Backend)?
                .into_iter()
                .map(|w| WindowSource {
                    id: native(w.id),
                    title: w.title,
                    app: w.app,
                    elevated: false,
                    refresh_hz: None,
                })
                .collect();
            let screens = self
                .x11_screens()?
                .into_iter()
                .map(|s| ScreenSource {
                    id: native(s.id),
                    name: s.name,
                    width: s.width,
                    height: s.height,
                    primary: s.primary,
                    refresh_hz: None,
                })
                .collect();
            return Ok(Sources { windows, screens });
        }
        Ok(Sources {
            windows: Vec::new(),
            screens: vec![ScreenSource {
                id: SHARED,
                name: "A window or screen, chosen in your desktop's dialog".into(),
                width: 0,
                height: 0,
                primary: true,
                refresh_hz: None,
            }],
        })
    }

    fn capture(
        &self,
        source: &Source,
        request: CaptureRequest,
    ) -> Result<(Box<dyn Capturer>, Option<NativeId>), MediaError> {
        match *source {
            Source::TestPattern { width, height } => Ok((
                Box::new(TestPattern::new(
                    width,
                    height,
                    request.fps.max(1),
                    Arc::new(MonotonicClock),
                )),
                None,
            )),
            _ if x11::session() => self.x11_capture(source, request.fps.max(1)),
            // `couch-cli host --source monitor` shares through the dialog too.
            Source::Screen(SHARED) | Source::PrimaryScreen => {
                Ok((self.share(request.fps.max(1), request.unattended)?, None))
            }
            Source::Screen(_) => Err(MediaError::Unsupported(
                "that screen: a Linux host shares what its desktop's dialog offers".into(),
            )),
            Source::Window(_) | Source::WindowTitle(_) => Err(MediaError::Unsupported(
                "a window by name: a Linux host shares what its desktop's dialog offers".into(),
            )),
        }
    }

    /// Frames in memory into the first Linux encoder that opens: VAAPI
    /// (AMD, Intel), NVENC, then software (`couch_codec_ffmpeg::probe`).
    fn encoder(
        &self,
        config: EncoderConfig,
        _gpu: Option<&GpuFrame>,
    ) -> Result<Box<dyn VideoEncoder>, MediaError> {
        // One encoder by FFmpeg's name, as on Windows: another machine's path
        // here (`libopenh264`, say).
        if let Ok(name) = std::env::var("COUCH_HOST_ENCODER")
            && !name.trim().is_empty()
        {
            tracing::info!("encoder {name}, frames in memory (COUCH_HOST_ENCODER)");
            return Ok(Box::new(FfmpegEncoder::open(name.trim(), config)?));
        }
        Ok(Box::new(FfmpegEncoder::open_best(config)?))
    }

    fn audio(
        &self,
        kind: AudioKind,
        stop: &AtomicBool,
    ) -> Result<Box<dyn AudioSource>, MediaError> {
        match kind {
            AudioKind::Tone => Ok(Box::new(ToneSource::new(Arc::new(MonotonicClock)))),
            // What the PC plays, this app left out.
            AudioKind::System => Ok(Box::new(SystemSound::start(Sounds::AllButOurs, stop)?)),
            AudioKind::Program(name) => {
                Ok(Box::new(SystemSound::start(Sounds::Program(name), stop)?))
            }
            // A window streamed on an X11 desktop (M28): what Wayland sends
            // for its one source, everything but this app (the window's own
            // sound by its process is for later, ADR 0015).
            AudioKind::Game(_) if x11::session() => {
                tracing::info!("audio: everything this PC plays, except this app");
                Ok(Box::new(SystemSound::start(Sounds::AllButOurs, stop)?))
            }
            AudioKind::Game(_) => Err(MediaError::Unsupported(
                "a window's own sound on Linux".into(),
            )),
        }
    }

    /// A virtual Xbox 360 controller through `uinput`, ready at once.
    fn plug_pad(&self, real_ids: bool) -> Result<Plugged, InputError> {
        let ids = if real_ids { XBOX_360_IDS } else { DEV_PAD_IDS };
        pads::UinputPads::new(ids).plug_pending()
    }

    /// Not drivers on Linux: whether this user may make input devices, and on
    /// GNOME our extension (M16c-2). Blocks briefly.
    fn drivers(&self) -> Vec<(DriverName, DriverState, bool)> {
        let state = if pads::available() {
            DriverState::Ready
        } else {
            DriverState::Missing
        };
        let mut drivers = vec![(DriverName::Uinput, state, true)];
        // On GNOME's X11 session the X server says what is in front (M28):
        // no extension needed.
        if !x11::session()
            && let Some(extension) = front::gnome::state()
        {
            drivers.push((DriverName::GnomeExtension, extension_state(extension), true));
        }
        drivers
    }

    fn virtual_pads(&self) -> bool {
        pads::available()
    }

    /// Asks for the administrator's password (`pkexec`) and installs one
    /// fixed udev rule.
    fn install_driver(&self, driver: DriverName) -> Result<(), String> {
        match driver {
            DriverName::Uinput => pads::set_up(),
            DriverName::GnomeExtension => match front::gnome::install()? {
                ExtensionState::Ready | ExtensionState::NeedsLogout => Ok(()),
                ExtensionState::ExtensionsOff => Err(
                    "GNOME's extensions are turned off: turn them on in its Extensions app, \n                     then try again"
                        .into(),
                ),
                state => Err(format!(
                    "GNOME didn't turn the extension on ({})",
                    match state {
                        ExtensionState::Failed => "this GNOME can't run it",
                        _ => "it stays off",
                    }
                )),
            },
            _ => Err("not on Linux".into()),
        }
    }

    /// A `uinput` keyboard and mouse (M16c-2), made at a session's first
    /// grant of either; nothing if this user can't make input devices (the
    /// capabilities say so first).
    fn kbm(&self) -> Box<dyn KbmInjector> {
        match UinputKbm::new() {
            Ok(kbm) => Box::new(kbm),
            Err(e) => {
                tracing::warn!("no keyboard and mouse for this session: {e}");
                Box::new(NoKbm)
            }
        }
    }

    /// As the desktop's helper last said; nothing in front while it hasn't.
    /// On an X11 desktop, as the X server last said (M28).
    fn focus(&self, window: NativeId) -> Focus {
        let (foreground, pointer_inside) = if x11::session() {
            u32::try_from(window).map_or((false, false), x11::tracker::focus)
        } else {
            front::focus(window)
        };
        Focus {
            foreground,
            pointer_inside,
        }
    }

    /// A named game window still listed by the helper; on an X11 desktop,
    /// a window the X server still has.
    fn window_exists(&self, window: NativeId) -> bool {
        if x11::session() {
            return u32::try_from(window).is_ok_and(x11::tracker::exists);
        }
        front::exists(window)
    }

    fn launch_viewer(&self, options: &ViewerOptions) -> Result<ViewerProcess, MediaError> {
        viewer_process::launch(options)
    }

    /// Only a remembered choice skips the screen-sharing dialog; the X
    /// server asks nothing (M28).
    fn shares_unattended(&self, source: &Source) -> bool {
        if x11::session() {
            return true;
        }
        match source {
            Source::Screen(SHARED) | Source::PrimaryScreen => self
                .restore_token
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some(),
            _ => true,
        }
    }

    /// Whether capture goes through the desktop's screen-sharing dialog:
    /// on Wayland, not on an X11 desktop (M28).
    fn shares_through_dialog(&self) -> bool {
        !x11::session()
    }

    fn remembers_shared_choice(&self) -> bool {
        self.restore_token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Our token goes, in the file too; the desktop's own record of it is
    /// left to expire unused. A dialog open now still gives the next token,
    /// if its "remember" is ticked: that choice is newer.
    fn forget_shared_choice(&self) {
        let had = self
            .restore_token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .is_some();
        save_token(self.portal_file.as_deref(), None);
        if had {
            tracing::info!("the shared window or screen is forgotten: the next stream asks");
        }
    }

    /// The game behind a window streamed on an X11 desktop (M28): its
    /// process, by the window's `_NET_WM_PID`, so the stream follows it to a
    /// new window as on Windows (a game switching full screen may replace
    /// its window). Never this app's own.
    fn game_process(&self, window: NativeId) -> Option<Box<dyn GameProcess>> {
        if !x11::session() {
            return None;
        }
        let pid = x11::pid_of(u32::try_from(window).ok()?)?;
        (pid != std::process::id()).then(|| Box::new(X11Game { pid }) as Box<dyn GameProcess>)
    }

    /// On an X11 desktop (M28) the window manager is asked, as a taskbar
    /// asks; Wayland lets no program raise another's window, and a Wayland
    /// stream is never a window anyway.
    fn bring_to_front(&self, window: NativeId) -> bool {
        if !x11::session() {
            return false;
        }
        let Ok(window) = u32::try_from(window) else {
            return false;
        };
        // Off the room actor, which never waits on the X server (M28's
        // review): asked, and the tracker sees whether it came.
        let asked = std::thread::Builder::new()
            .name("x11-activate".into())
            .spawn(move || {
                if let Err(e) = x11::activate(window) {
                    tracing::warn!("the window couldn't be brought to the front: {e}");
                }
            });
        asked.is_ok()
    }

    fn play_chime(&self) {
        couch_platform_linux::chime::play();
    }

    fn copy_secret(&self, text: &str) -> Result<(), MediaError> {
        couch_platform_linux::clipboard::copy_secret(text)
            .map_err(|e| MediaError::Backend(format!("the clipboard: {e}")))
    }

    fn open_folder(&self, dir: &std::path::Path) -> Result<(), MediaError> {
        couch_platform_linux::folder::open(dir, "desktop.log").map_err(MediaError::Backend)
    }

    fn open_page(&self, url: &str) -> Result<(), MediaError> {
        couch_platform_linux::web::open(url).map_err(MediaError::Backend)
    }

    /// The desktop's dialog comes when the host starts the stream, once:
    /// never over the game when a guest arrives, nor again each time
    /// someone comes back. The X server asks nothing (M28): it captures
    /// only while someone watches, as on Windows.
    fn captures_without_viewers(&self) -> bool {
        !x11::session()
    }

    /// The streamed screen as the X server last listed it (M28), where
    /// desktop control keeps the pointer; nothing on Wayland, which tells
    /// no program where the pointer is.
    fn screen_area(&self, source: &Source) -> Option<ScreenArea> {
        let Source::Screen(id) = source else {
            return None;
        };
        if !x11::session() {
            return None;
        }
        let id = u32::try_from(*id).ok()?;
        self.screens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| {
                Some(ScreenArea {
                    x: s.x,
                    y: s.y,
                    width: i32::try_from(s.width).ok()?,
                    height: i32::try_from(s.height).ok()?,
                })
            })
    }

    /// The portal doesn't say whose the shared window is: the host picks a
    /// program, or sends everything but this app.
    fn picks_sound(&self) -> bool {
        true
    }

    fn sound_programs(&self) -> Result<Vec<String>, MediaError> {
        couch_platform_linux::sound::programs()
    }

    fn show_notices(&self, list: Vec<NoticeView>) -> Result<(), MediaError> {
        let list = list
            .into_iter()
            .map(|n| notices::Notice {
                id: n.id,
                title: n.title,
                body: n.body,
                lasting: n.lasting,
            })
            .collect();
        notices::show(list).map_err(MediaError::Backend)
    }

    fn close_notices(&self) {
        notices::close_all(Duration::from_millis(500));
    }

    fn keep_awake(&self, display: bool) -> Option<Box<dyn Any + Send>> {
        match awake::keep("Couch Co-App: remote play is on", display) {
            Ok(awake) => Some(Box::new(awake)),
            Err(e) => {
                tracing::warn!("this PC can't be kept awake: {e}");
                None
            }
        }
    }

    fn screen_locked(&self) -> bool {
        session_lock::locked()
    }

    /// An AppImage's own autostart entry: nothing else on Linux starts at
    /// sign-in.
    fn start_at_sign_in(&self) -> Result<bool, String> {
        let appimage = autostart::own_appimage().ok_or(NOT_AN_APPIMAGE)?;
        autostart::is_set(&appimage).map_err(|e| e.to_string())
    }

    fn set_start_at_sign_in(&self, on: bool) -> Result<(), String> {
        let appimage = autostart::own_appimage().ok_or(NOT_AN_APPIMAGE)?;
        autostart::set(&appimage, on).map_err(|e| e.to_string())
    }

    /// A newer AppImage (its name carries the version) takes over the entry
    /// an older one left, naming a file that's gone.
    fn keep_start_at_sign_in(&self) {
        let Some(appimage) = autostart::own_appimage() else {
            return;
        };
        match autostart::follow(&appimage) {
            Ok(true) => tracing::info!("starting at sign-in: now this AppImage"),
            Ok(false) => {}
            Err(e) => tracing::warn!("the autostart entry couldn't follow this AppImage: {e}"),
        }
    }

    /// Through the desktop's global-shortcuts portal, each key with Ctrl,
    /// Alt and Shift as the preferred trigger (the desktop may let the user
    /// change it). The portal can't tell a guest's injected keys from the host's
    /// (`host_keys_hear_injected`: the router and the session keep guests off
    /// them instead). Returns at
    /// once: the desktop's answer comes later, as `Bound` or `Lost`.
    ///
    /// On an X11 desktop (M28-2) the portal is tried first (KDE's works
    /// there); where it fails (Mint, Xfce, MATE have none, or nobody
    /// answered its dialog), the keys are grabbed from the X server itself
    /// (`x11::keys`), which hears injected keys as the portal does.
    fn host_keys(
        &self,
        keys: &[(HidUsage, Shortcut)],
        unattended: bool,
        on_event: Box<dyn Fn(HostKeyEvent) + Send>,
    ) -> Result<Box<dyn Any + Send>, MediaError> {
        let offered: Vec<global_keys::Key> = keys
            .iter()
            .filter_map(|&(usage, shortcut)| {
                let (id, description) = host_key(shortcut)?;
                Some(global_keys::Key {
                    id,
                    description,
                    trigger: global_keys::chord(usage.0)?,
                })
            })
            .collect();
        // Called from the portal's runtime and, should it fail on X11, the
        // X server's grab.
        let send = {
            let emit = Arc::new(Mutex::new(on_event));
            move |event: HostKeyEvent| (emit.lock().unwrap_or_else(PoisonError::into_inner))(event)
        };
        let answer_within = unattended.then_some(super::UNANSWERED_AFTER);
        if !x11::session() {
            let reported = move |event: global_keys::Event<'_>| {
                if let Some(event) = host_key_event(event) {
                    send(event);
                }
            };
            return global_keys::bind(offered, answer_within, Box::new(reported))
                .map(|keys| Box::new(keys) as Box<dyn Any + Send>)
                .map_err(MediaError::Unsupported);
        }
        // The X server's grab, once, should the portal fail.
        let grabbed: Arc<Mutex<Option<x11::keys::Grabbed>>> = Arc::default();
        let fallback = Arc::new({
            let slot = Arc::downgrade(&grabbed);
            let send = send.clone();
            let x11_keys = x11_keys_from(keys);
            let once = std::sync::Once::new();
            move |why: String| {
                once.call_once(|| {
                    let (slot, send, keys) = (slot.clone(), send.clone(), x11_keys.clone());
                    // Off the portals' runtime: the grabs wait on the server.
                    std::thread::spawn(move || x11_host_keys(why, keys, &slot, send));
                });
            }
        });
        let reported = {
            let fallback = fallback.clone();
            move |event: global_keys::Event<'_>| match event {
                global_keys::Event::Failed(why) => fallback(why),
                event => {
                    if let Some(event) = host_key_event(event) {
                        send(event);
                    }
                }
            }
        };
        let portal = global_keys::bind(offered, answer_within, Box::new(reported));
        if let Err(why) = &portal {
            // No portal at all: the X server's at once.
            fallback(why.clone());
        }
        Ok(Box::new(X11HostKeys {
            _portal: portal.ok(),
            _grabbed: grabbed,
        }))
    }
}

/// A game on an X11 desktop, by its process (M28).
struct X11Game {
    pid: u32,
}

impl GameProcess for X11Game {
    fn running(&self) -> bool {
        std::path::Path::new(&format!("/proc/{}", self.pid)).exists()
    }

    /// Its windows the window manager lists, the newest first.
    fn windows(&self) -> Vec<NativeId> {
        x11::windows()
            .unwrap_or_default()
            .into_iter()
            .rev()
            .filter(|w| w.pid == Some(self.pid))
            .map(|w| native(w.id))
            .collect()
    }
}

/// An X11 desktop's host keys: the portal's, or the X server's grab, until
/// dropped.
struct X11HostKeys {
    _portal: Option<global_keys::GlobalKeys>,
    _grabbed: Arc<Mutex<Option<x11::keys::Grabbed>>>,
}

/// The X11 grab's view of the host keys: each one's id, evdev code and
/// description.
fn x11_keys_from(keys: &[(HidUsage, Shortcut)]) -> Vec<(&'static str, u16, String)> {
    keys.iter()
        .filter_map(|&(usage, shortcut)| {
            let (id, _) = host_key(shortcut)?;
            Some((
                id,
                couch_platform_linux::kbm::key_code(usage.0)?,
                global_keys::described(usage.0)?,
            ))
        })
        .collect()
}

/// Grabs the host's keys from the X server, the portal having failed for
/// `why`, and keeps them in `slot` while its keys are wanted.
fn x11_host_keys(
    why: String,
    keys: Vec<(&'static str, u16, String)>,
    slot: &std::sync::Weak<Mutex<Option<x11::keys::Grabbed>>>,
    send: impl Fn(HostKeyEvent) + Send + Clone + 'static,
) {
    if slot.strong_count() == 0 {
        return;
    }
    tracing::info!(
        "the desktop gives no global shortcuts ({why}): the host's keys from the X server"
    );
    let keys = keys
        .into_iter()
        .map(|(id, code, described)| x11::keys::Key {
            id,
            code,
            described,
        })
        .collect();
    let reported = {
        let send = send.clone();
        move |event: global_keys::Event<'_>| {
            if let Some(event) = host_key_event(event) {
                send(event);
            }
        }
    };
    match x11::keys::grab(keys, Box::new(reported)) {
        // Kept only while the keys are still wanted: dropped here otherwise,
        // which lets them go.
        Ok(grabbed) => {
            if let Some(slot) = slot.upgrade() {
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(grabbed);
            }
        }
        Err(e) => send(HostKeyEvent::Lost(format!(
            "{why}; nor from the X server: {e}"
        ))),
    }
}

/// A host key's report as the engine's, by its id.
fn host_key_event(event: global_keys::Event<'_>) -> Option<HostKeyEvent> {
    let shortcut = |id: &str| {
        Shortcut::HOST
            .iter()
            .copied()
            .find(|&s| host_key(s).map(|k| k.0) == Some(id))
    };
    Some(match event {
        global_keys::Event::Pressed(id) => HostKeyEvent::Pressed(shortcut(id)?),
        global_keys::Event::Bound(bound) => HostKeyEvent::Bound(
            bound
                .into_iter()
                .filter_map(|(id, keys)| Some((shortcut(&id)?, keys)))
                .collect(),
        ),
        global_keys::Event::Failed(e) => HostKeyEvent::Lost(e),
    })
}

/// The GNOME extension's state as Settings → Drivers shows it.
fn extension_state(state: ExtensionState) -> DriverState {
    match state {
        ExtensionState::Missing => DriverState::Missing,
        ExtensionState::Outdated => DriverState::Outdated,
        ExtensionState::NeedsLogout => DriverState::NeedsLogout,
        ExtensionState::Off => DriverState::Off,
        ExtensionState::ExtensionsOff => DriverState::ExtensionsOff,
        ExtensionState::Failed => DriverState::Unsupported,
        ExtensionState::Ready => DriverState::Ready,
    }
}

/// A host key's id and what it does, as the desktop shows them.
fn host_key(shortcut: Shortcut) -> Option<(&'static str, &'static str)> {
    Some(match shortcut {
        Shortcut::Panic => (
            "panic",
            "Couch Co-App: take every seat back from guests and pause control",
        ),
        Shortcut::AcceptRequest => ("acceptRequest", "Couch Co-App: accept a guest's request"),
        Shortcut::DeclineRequest => ("declineRequest", "Couch Co-App: decline a guest's request"),
        _ => return None,
    })
}
