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
};

use super::{
    AudioKind, Backend, CaptureRequest, Focus, GameWindow, HostKeyEvent, NativeId, NoKbm,
    ScreenSource, Source, Sources, ViewerOptions, ViewerProcess, viewer_process,
};
use crate::{DriverName, DriverState, HostCapabilities, NoticeView};

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
        }
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
    /// last looked.
    fn capabilities(&self) -> HostCapabilities {
        let devices = pads::available();
        HostCapabilities {
            sessions: true,
            keyboard_seat: devices && front::available().is_some(),
            desktop_control: devices,
        }
    }

    /// The portal never says which window it shares: the host names the
    /// game.
    fn names_game_window(&self) -> bool {
        true
    }

    /// The KWin script loaded, or our GNOME extension asked to watch.
    fn watch_front(&self) -> Option<Box<dyn Any + Send>> {
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
        front::available() == Some(front::Helper::KWin)
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
    /// stream starts.
    fn sources(&self) -> Result<Sources, MediaError> {
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
        if let Some(extension) = front::gnome::state() {
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
    fn focus(&self, window: NativeId) -> Focus {
        let (foreground, pointer_inside) = front::focus(window);
        Focus {
            foreground,
            pointer_inside,
        }
    }

    /// A named game window still listed by the helper.
    fn window_exists(&self, window: NativeId) -> bool {
        front::exists(window)
    }

    fn launch_viewer(&self, options: &ViewerOptions) -> Result<ViewerProcess, MediaError> {
        viewer_process::launch(options)
    }

    /// Only a remembered choice skips the screen-sharing dialog.
    fn shares_unattended(&self, source: &Source) -> bool {
        match source {
            Source::Screen(SHARED) | Source::PrimaryScreen => self
                .restore_token
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some(),
            _ => true,
        }
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

    /// The desktop's dialog comes when the host starts the stream, once:
    /// never over the game when a guest arrives, nor again each time
    /// someone comes back.
    fn captures_without_viewers(&self) -> bool {
        true
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
        let shortcut = |id: &str| {
            Shortcut::HOST
                .iter()
                .copied()
                .find(|&s| host_key(s).map(|k| k.0) == Some(id))
        };
        let reported = move |event: global_keys::Event<'_>| match event {
            global_keys::Event::Pressed(id) => {
                if let Some(s) = shortcut(id) {
                    on_event(HostKeyEvent::Pressed(s));
                }
            }
            global_keys::Event::Bound(bound) => on_event(HostKeyEvent::Bound(
                bound
                    .into_iter()
                    .filter_map(|(id, keys)| Some((shortcut(&id)?, keys)))
                    .collect(),
            )),
            global_keys::Event::Failed(e) => on_event(HostKeyEvent::Lost(e)),
        };
        let answer_within = unattended.then_some(super::UNANSWERED_AFTER);
        global_keys::bind(offered, answer_within, Box::new(reported))
            .map(|keys| Box::new(keys) as Box<dyn Any + Send>)
            .map_err(MediaError::Unsupported)
    }
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
