//! Sessions end to end: three engines over real iroh on loopback, in one
//! room, with a fake backend instead of the machine: gray frames, an encoder
//! that emits tiny access units, a recording pad and injector, and an
//! in-process viewer that speaks the viewer pipe. The rules are the real
//! ones; nothing here needs a GPU or a gamepad driver.

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use couch_engine::{
    Command, Engine, EngineError, EngineOptions, Event, MemberLink, Profile, Response, RoomView,
    SessionEnd, SessionRole, SessionView, SourceChoice, WatchEnd,
    pipeline::{
        AudioKind, Backend, Focus, NativeId, Source, Sources, ViewerOptions, ViewerProcess,
        WindowSource, ipc,
    },
    secrets::Unprotected,
};
use couch_input::{InputError, KbmInjector, PadButtons, PadState, VirtualPad, VirtualPadFactory};
use couch_media::{
    AudioSource, Capturer, EncoderConfig, MediaError, VideoEncoder,
    audio::ToneSource,
    clock::{Clock, ProcessClock},
};
use couch_net::endpoint::RelayChoice;
use couch_proto::{
    input::PadSnapshot,
    ipc::{HudState, LocalInput, SeatState, UserAction, VideoFrameHeader},
};
use couch_room::{limits::LimitConfig, room::Timings};
use couch_testkit::{FakeCapturer, FakeEncoder, RecordingInjector, RecordingPadFactory};
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};

const PASSWORD: &str = "pizza night";
const WAIT: Duration = Duration::from_secs(20);
/// The fake window everyone streams.
const WINDOW: NativeId = 7;

// ---- The fake machine ----

#[derive(Default)]
struct Fake {
    kbm: RecordingInjector,
    pads: RecordingPadFactory,
    viewers: Mutex<Vec<FakeViewer>>,
    /// The window closed: capture says so, and it no longer exists.
    closed: Arc<AtomicBool>,
    clock: Arc<ProcessClock>,
    /// The panic key's handler while the engine has it hooked.
    panic_key: PanicHook,
}

impl Fake {
    /// The pad's latest state, if one was plugged.
    fn pad(&self) -> Option<PadState> {
        let plugged = self.pads.plugged.lock().unwrap();
        plugged.last()?.states().last().copied()
    }

    fn viewer(&self) -> Option<FakeViewer> {
        self.viewers.lock().unwrap().last().cloned()
    }
}

impl Backend for Fake {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    fn sources(&self) -> Result<Sources, MediaError> {
        Ok(Sources {
            windows: vec![WindowSource {
                id: WINDOW,
                title: "Celeste".into(),
                app: "Celeste.exe".into(),
            }],
            screens: Vec::new(),
        })
    }

    fn capture(
        &self,
        source: &Source,
        fps: u32,
    ) -> Result<(Box<dyn Capturer>, Option<NativeId>), MediaError> {
        let window = matches!(source, Source::Window(_)).then_some(WINDOW);
        let capturer = FakeCapturer::new(64, 36, fps, self.clock.clone(), self.closed.clone());
        Ok((Box::new(capturer), window))
    }

    fn encoder(&self, _config: EncoderConfig) -> Result<Box<dyn VideoEncoder>, MediaError> {
        Ok(Box::new(FakeEncoder::default()))
    }

    fn audio(&self, _kind: AudioKind) -> Result<Box<dyn AudioSource>, MediaError> {
        Ok(Box::new(ToneSource::new(self.clock.clone())))
    }

    fn plug_pad(&self, _real_ids: bool) -> Result<Box<dyn VirtualPad>, InputError> {
        self.pads.plug()
    }

    fn kbm(&self) -> Box<dyn KbmInjector> {
        Box::new(self.kbm.clone())
    }

    fn focus(&self, _window: NativeId) -> Focus {
        Focus {
            foreground: true,
            pointer_inside: true,
        }
    }

    fn window_exists(&self, _window: NativeId) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    fn launch_viewer(&self, _options: &ViewerOptions) -> Result<ViewerProcess, MediaError> {
        let (viewer, process) = FakeViewer::launch();
        self.viewers.lock().unwrap().push(viewer);
        Ok(process)
    }

    fn panic_key(
        &self,
        on_press: Box<dyn Fn() + Send>,
    ) -> Result<Box<dyn std::any::Any + Send>, MediaError> {
        *self.panic_key.lock().unwrap() = Some(on_press);
        Ok(Box::new(Unhook(self.panic_key.clone())))
    }
}

type PanicHook = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

/// Removes the fake panic-key hook when the engine drops it.
struct Unhook(PanicHook);

impl Drop for Unhook {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

impl Fake {
    /// The host presses the panic key on its keyboard; `false` if no hook
    /// was in place to see it.
    fn press_panic_key(&self) -> bool {
        match &*self.panic_key.lock().unwrap() {
            Some(on_press) => {
                on_press();
                true
            }
            None => false,
        }
    }

    fn panic_key_hooked(&self) -> bool {
        self.panic_key.lock().unwrap().is_some()
    }
}

/// Pipe frames the fake viewer sends: (kind, payload).
type Said = mpsc::UnboundedSender<(u8, Vec<u8>)>;

/// A viewer window, in-process: it counts the frames it gets, keeps the
/// seat and HUD state, says it is alive every 100 ms, and sends the input
/// and actions a test asks for.
#[derive(Clone, Default)]
struct FakeViewer {
    frames: Arc<AtomicU64>,
    first_was_key: Arc<Mutex<Option<bool>>>,
    seat: Arc<Mutex<Option<SeatState>>>,
    hud: Arc<Mutex<Option<HudState>>>,
    /// Pipe frames to send back: (kind, payload).
    say: Arc<Mutex<Option<Said>>>,
    shut_down: Arc<AtomicBool>,
}

impl FakeViewer {
    fn launch() -> (Self, ViewerProcess) {
        let (main_in, mut viewer_in) = tokio::io::duplex(1 << 20);
        let (mut viewer_out, main_out) = tokio::io::duplex(1 << 20);
        let (say, mut to_say) = mpsc::unbounded_channel::<(u8, Vec<u8>)>();
        let viewer = Self::default();
        *viewer.say.lock().unwrap() = Some(say);
        let v = viewer.clone();
        tokio::spawn(async move {
            let mut alive = tokio::time::interval(Duration::from_millis(100));
            let mut scratch = Vec::new();
            loop {
                tokio::select! {
                    frame = ipc::read_frame(&mut viewer_in) => {
                        let Ok(Some(frame)) = frame else { break };
                        match frame.kind {
                            couch_proto::ipc::kinds::VIDEO_FRAME => {
                                if let Some((header, _)) = VideoFrameHeader::read(&frame.payload) {
                                    v.first_was_key.lock().unwrap().get_or_insert(header.keyframe);
                                    v.frames.fetch_add(1, Ordering::SeqCst);
                                }
                            }
                            couch_proto::ipc::kinds::SEAT => {
                                *v.seat.lock().unwrap() = couch_proto::ipc::from_payload(&frame.payload);
                            }
                            couch_proto::ipc::kinds::HUD_STATE => {
                                *v.hud.lock().unwrap() = couch_proto::ipc::from_payload(&frame.payload);
                            }
                            couch_proto::ipc::kinds::SHUTDOWN => {
                                v.shut_down.store(true, Ordering::SeqCst);
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = alive.tick() => {
                        let payload = couch_proto::ipc::to_payload(&LocalInput::Alive);
                        if ipc::write_frame(&mut viewer_out, couch_proto::ipc::kinds::INPUT, &payload, &mut scratch).await.is_err() {
                            break;
                        }
                    }
                    said = to_say.recv() => {
                        // `None`: the window was closed.
                        let Some((kind, payload)) = said else { break };
                        if ipc::write_frame(&mut viewer_out, kind, &payload, &mut scratch).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let process = ViewerProcess {
            stdin: Box::pin(main_in),
            stdout: Box::pin(main_out),
            guard: Box::new(()),
        };
        (viewer, process)
    }

    fn say(&self, kind: u8, payload: Vec<u8>) {
        if let Some(say) = &*self.say.lock().unwrap() {
            let _ = say.send((kind, payload));
        }
    }

    /// The local gamepad, with these buttons down.
    fn pad(&self, buttons: PadButtons) {
        let pad = LocalInput::Pad(PadSnapshot {
            buttons: buttons.bits(),
            ..Default::default()
        });
        self.say(
            couch_proto::ipc::kinds::INPUT,
            couch_proto::ipc::to_payload(&pad),
        );
    }

    fn action(&self, action: UserAction) {
        self.say(
            couch_proto::ipc::kinds::USER_ACTION,
            couch_proto::ipc::to_payload(&action),
        );
    }

    /// The user closes the window.
    fn close(&self) {
        self.say.lock().unwrap().take();
    }

    fn frames(&self) -> u64 {
        self.frames.load(Ordering::SeqCst)
    }

    fn holding(&self) -> bool {
        self.seat.lock().unwrap().is_some_and(|s| s.holding)
    }
}

// ---- Members ----

fn options(port: u16, fake: Arc<Fake>) -> EngineOptions {
    EngineOptions {
        port: Some(port),
        relays: RelayChoice::Disabled,
        secret_store: Arc::new(Unprotected),
        limits: LimitConfig::default(),
        timings: Timings {
            repair_every: Duration::from_secs(2),
            first_redial: Duration::from_millis(200),
            max_redial: Duration::from_secs(2),
            after_shutdown: Duration::from_secs(1),
        },
        tick: Duration::from_millis(50),
        invite_valid_for: Duration::from_secs(3600),
        profile_wait: Duration::from_secs(2),
        backend: fake,
        join_wait: Duration::from_secs(5),
    }
}

struct Member {
    _dir: TempDir,
    name: &'static str,
    engine: Option<Engine>,
    fake: Arc<Fake>,
    events: broadcast::Receiver<Event>,
}

impl Member {
    async fn new(name: &'static str) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let engine = start(dir.path(), options(free_port(), fake.clone())).await;
        engine
            .handle(Command::SetDisplayName { name: name.into() })
            .await
            .unwrap();
        let events = engine.subscribe();
        Self {
            _dir: dir,
            name,
            engine: Some(engine),
            fake,
            events,
        }
    }

    fn engine(&self) -> &Engine {
        self.engine.as_ref().expect("running")
    }

    fn id(&self) -> String {
        self.engine().member_id().to_string()
    }

    async fn handle(&self, command: Command) -> Result<Response, EngineError> {
        self.engine().handle(command).await
    }

    async fn ok(&self, command: Command) {
        self.handle(command).await.unwrap();
    }

    async fn room(&self, id: &str) -> Option<RoomView> {
        match self.handle(Command::ListRooms).await.unwrap() {
            Response::Rooms { rooms } => rooms.into_iter().find(|r| r.id == id),
            other => panic!("expected rooms, got {other:?}"),
        }
    }

    /// `host`'s session in the room, as this member sees it.
    async fn session(&self, room: &str, host: &Member) -> Option<SessionView> {
        let host = host.id();
        self.room(room)
            .await?
            .sessions
            .into_iter()
            .find(|s| s.host == host)
    }

    async fn wait_session(
        &self,
        room: &str,
        host: &Member,
        what: &str,
        check: impl Fn(Option<&SessionView>) -> bool,
    ) {
        let deadline = Instant::now() + WAIT;
        loop {
            let session = self.session(room, host).await;
            if check(session.as_ref()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: timed out waiting for {what}; last: {session:#?}",
                self.name
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn go_live(&self, room: &str, max_viewers: u8) {
        let reply = self.handle(go_live(room, max_viewers)).await;
        assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    }

    async fn watch(&self, room: &str, host: &Member) -> Result<Response, EngineError> {
        self.handle(Command::Watch {
            room: room.into(),
            host: host.id(),
        })
        .await
    }

    /// The next event `pick` accepts, within the wait.
    async fn event<T>(&mut self, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("{}: no {what}", self.name))
                .expect("events flow");
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    }

    async fn stop(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown().await;
        }
    }
}

async fn start(dir: &Path, options: EngineOptions) -> Engine {
    Engine::start(Profile::new("test", dir).unwrap(), options)
        .await
        .expect("engine starts")
}

fn go_live(room: &str, max_viewers: u8) -> Command {
    Command::GoLive {
        room: room.into(),
        source: SourceChoice::Window {
            id: WINDOW.to_string(),
        },
        title: "Celeste".into(),
        fps: 30,
        max_mbps: 20,
        upload_mbps: None,
        max_viewers,
        keyboard_mouse: false,
    }
}

/// A (the owner) with B and C, all connected.
async fn three() -> (Member, Member, Member, String) {
    let a = Member::new("Ana").await;
    let b = Member::new("Bia").await;
    let c = Member::new("Caio").await;
    let Response::Room(room) = a
        .handle(Command::CreateRoom {
            name: "Sofá".into(),
            password: PASSWORD.into(),
        })
        .await
        .unwrap()
    else {
        panic!("a room");
    };
    let Response::Invite { link } = a
        .handle(Command::GetInvite {
            room: room.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("an invite");
    };
    for m in [&b, &c] {
        m.ok(Command::JoinRoom {
            invite: link.clone(),
            password: PASSWORD.into(),
        })
        .await;
    }
    for m in [&a, &b, &c] {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = m.room(&room.id).await.unwrap();
            let linked = view
                .members
                .iter()
                .filter(|x| matches!(x.link, MemberLink::Direct | MemberLink::Relay))
                .count();
            if view.members.len() == 3 && linked == 2 {
                break;
            }
            assert!(Instant::now() < deadline, "{}: no full mesh", m.name);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    (a, b, c, room.id)
}

/// `viewer` watches `host`'s live session; returns its viewer window once
/// frames arrive.
async fn watching(room: &str, host: &Member, viewer: &Member) -> FakeViewer {
    viewer
        .wait_session(room, host, "the announcement", |s| s.is_some())
        .await;
    let reply = viewer.watch(room, host).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let window = viewer.fake.viewer().expect("a viewer window");
    wait("frames", || window.frames() >= 5).await;
    window
}

async fn wait(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// B asks, A accepts: B plays.
async fn give_control(room: &str, a: &mut Member, b: &Member, window: &FakeViewer) {
    b.ok(Command::RequestControl).await;
    let asker = a
        .event("a request", |e| match e {
            Event::ControlRequested { member, .. } => Some(member.clone()),
            _ => None,
        })
        .await;
    assert_eq!(asker, b.id());
    a.ok(Command::AnswerControl {
        member: b.id(),
        accept: true,
    })
    .await;
    wait("the seat", || window.holding()).await;
    a.wait_session(room, a, "B as the controller", |s| {
        s.is_some_and(|s| s.controller.as_deref() == Some(&b.id()))
    })
    .await;
}

fn pressed(buttons: PadButtons) -> PadState {
    PadState {
        buttons,
        ..PadState::NEUTRAL
    }
}

async fn stop_all(members: &mut [&mut Member]) {
    for m in members.iter_mut() {
        m.stop().await;
    }
}

/// A UDP port free on both IPv4 and IPv6 right now.
fn free_port() -> u16 {
    loop {
        let v4 = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        if std::net::UdpSocket::bind(("::", port)).is_ok() {
            return port;
        }
    }
}

fn refused_because(reply: &Result<Response, EngineError>, words: &str) -> bool {
    matches!(reply, Err(EngineError::Refused(why)) if why.contains(words))
}

// ---- Scenarios ----

#[tokio::test(flavor = "multi_thread")]
async fn two_viewers_each_get_a_stream_of_their_own() {
    let (mut a, mut b, mut c, room) = three().await;
    let Response::Sources(sources) = a.handle(Command::ListSources).await.unwrap() else {
        panic!("sources");
    };
    assert_eq!(sources.windows[0].title, "Celeste");
    a.go_live(&room, 4).await;

    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;
    assert_eq!(*b_window.first_was_key.lock().unwrap(), Some(true));
    assert_eq!(*c_window.first_was_key.lock().unwrap(), Some(true));
    let (b_frames, c_frames) = (b_window.frames(), c_window.frames());
    wait("frames keep coming to both", || {
        b_window.frames() > b_frames + 5 && c_window.frames() > c_frames + 5
    })
    .await;

    let (bid, cid) = (b.id(), c.id());
    a.wait_session(&room, &a, "two viewers", |s| {
        s.is_some_and(|s| s.viewers == [bid.clone(), cid.clone()] && s.role == SessionRole::Host)
    })
    .await;
    // Everyone in the room sees who watches.
    c.wait_session(&room, &a, "two viewers, from C", |s| {
        s.is_some_and(|s| s.viewers.len() == 2 && s.role == SessionRole::Watching)
    })
    .await;
    // Asking again changes nothing.
    assert!(matches!(b.watch(&room, &a).await, Ok(Response::Room(_))));
    // The HUD knows the session.
    wait("a HUD", || b_window.hud.lock().unwrap().is_some()).await;
    let hud = b_window.hud.lock().unwrap().clone().unwrap();
    assert_eq!((hud.title.as_str(), hud.host.as_str()), ("Celeste", "Ana"));
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_session_turns_one_away_and_one_stream_at_a_time() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 1).await;
    watching(&room, &a, &b).await;
    c.wait_session(&room, &a, "the announcement", |s| s.is_some())
        .await;
    let reply = c.watch(&room, &a).await;
    assert!(refused_because(&reply, "all the viewers"), "{reply:?}");

    // One stream per profile, never while watching, and keyboard and mouse
    // only for a window.
    assert!(refused_because(
        &a.handle(go_live(&room, 1)).await,
        "streaming already"
    ));
    assert!(refused_because(
        &b.handle(go_live(&room, 1)).await,
        "stop watching first"
    ));
    let screen_with_kbm = Command::GoLive {
        room: room.clone(),
        source: SourceChoice::Screen { id: "1".into() },
        title: "Desktop".into(),
        fps: 30,
        max_mbps: 20,
        upload_mbps: None,
        max_viewers: 1,
        keyboard_mouse: true,
    };
    assert!(refused_because(
        &c.handle(screen_with_kbm).await,
        "need a window"
    ));
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_is_asked_for_given_passed_and_given_back() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;

    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::A);
    wait("B's button on A's pad", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;
    // C watches but doesn't hold the seat: its pad goes nowhere.
    c_window.pad(PadButtons::B);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.fake.pad(), Some(pressed(PadButtons::A)));

    // A passes control to C: B's hold ends, released first.
    a.ok(Command::PassControl { member: c.id() }).await;
    wait("C's seat", || c_window.holding()).await;
    wait("B's seat gone", || !b_window.holding()).await;
    wait("C's button", || {
        a.fake.pad() == Some(pressed(PadButtons::B))
    })
    .await;
    let states = a.fake.pads.plugged.lock().unwrap()[0].states();
    let a_up = states
        .iter()
        .rposition(|s| *s == pressed(PadButtons::A))
        .unwrap();
    let b_down = states
        .iter()
        .rposition(|s| *s == pressed(PadButtons::B))
        .unwrap();
    assert!(
        states[a_up..b_down].contains(&PadState::NEUTRAL),
        "neutral between holders: {states:?}"
    );
    // B's input no longer counts.
    b_window.pad(PadButtons::X);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.fake.pad(), Some(pressed(PadButtons::B)));

    // C gives it back from the viewer: everything released.
    c_window.action(UserAction::CancelControl);
    wait("C's seat gone", || !c_window.holding()).await;
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    a.wait_session(&room, &a, "the host playing", |s| {
        s.is_some_and(|s| s.controller.is_none())
    })
    .await;

    // A takes it back from someone it passed it to.
    a.ok(Command::PassControl { member: b.id() }).await;
    wait("B's seat again", || b_window.holding()).await;
    a.ok(Command::TakeBackControl).await;
    wait("B's seat gone again", || !b_window.holding()).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_viewer_cannot_ask_again_at_once() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    b_window.action(UserAction::RequestControl);
    a.event("a request", |e| {
        matches!(e, Event::ControlRequested { .. }).then_some(())
    })
    .await;
    a.ok(Command::AnswerControl {
        member: b.id(),
        accept: false,
    })
    .await;
    b.wait_session(&room, &a, "B watching again", |s| {
        s.is_some_and(|s| s.role == SessionRole::Watching)
    })
    .await;
    wait("the HUD's note", || {
        b_window
            .hud
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|h| h.note.contains("said no"))
    })
    .await;

    // Straight away: declined again, and A never hears of it.
    b.ok(Command::RequestControl).await;
    b.wait_session(&room, &a, "the second request declined", |s| {
        s.is_some_and(|s| s.role == SessionRole::Watching)
    })
    .await;
    let requests = a.session(&room, &a).await.unwrap().requests;
    assert!(requests.is_empty(), "{requests:?}");
    assert!(!b_window.holding());
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panic_key_takes_control_back_until_allowed() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::A);
    wait("B's button", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;

    assert_eq!(
        a.session(&room, &a).await.unwrap().panic_key.as_deref(),
        Some("Ctrl+Alt+Shift+Backspace")
    );
    // Pressed on the keyboard: the hook, not the app.
    assert!(a.fake.press_panic_key(), "the hook is in place while live");
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    wait("B's seat gone", || !b_window.holding()).await;
    a.wait_session(&room, &a, "control paused", |s| {
        s.is_some_and(|s| s.control_paused && s.controller.is_none())
    })
    .await;
    b.wait_session(&room, &a, "B hears control is paused", |s| {
        s.is_some_and(|s| s.control_paused)
    })
    .await;
    // B's held button stays released, and asking is declined.
    b_window.pad(PadButtons::A | PadButtons::B);
    b.ok(Command::RequestControl).await;
    b.wait_session(&room, &a, "B's request declined", |s| {
        s.is_some_and(|s| s.role == SessionRole::Watching)
    })
    .await;
    assert_eq!(a.fake.pad(), Some(PadState::NEUTRAL));
    assert!(matches!(
        a.handle(Command::PassControl { member: b.id() }).await,
        Err(EngineError::Session(_))
    ));

    a.ok(Command::AllowControl).await;
    b.wait_session(&room, &a, "control allowed again", |s| {
        s.is_some_and(|s| !s.control_paused)
    })
    .await;
    give_control(&room, &mut a, &b, &b_window).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_closing_its_window_while_playing_is_released_at_once() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::Y);
    wait("B's button", || {
        a.fake.pad() == Some(pressed(PadButtons::Y))
    })
    .await;

    let closed = Instant::now();
    b_window.close();
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    let released = closed.elapsed();
    a.wait_session(&room, &a, "B gone from the viewers", |s| {
        s.is_some_and(|s| s.viewers.is_empty() && s.controller.is_none())
    })
    .await;
    b.wait_session(&room, &a, "B not watching", |s| {
        s.is_some_and(|s| s.role == SessionRole::None)
    })
    .await;
    // Through the session, not the dead-man timer: B's last heartbeat went
    // out at most 100 ms before the close, so the timer can't fire before
    // about 200 ms after it. The session takes a few ms on loopback.
    assert!(released < Duration::from_millis(150), "{released:?}");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ban_while_watching_ends_it() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::A);
    wait("B's button", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;

    a.ok(Command::BanMember {
        room: room.clone(),
        member: b.id(),
    })
    .await;
    // Dropped in the same step as the ban, not when B's connection finally
    // closes (which B could delay), and released before B's heartbeats
    // could stop and let the dead-man timer do it.
    let banned = Instant::now();
    let session = a.session(&room, &a).await.unwrap();
    assert!(session.viewers.is_empty(), "{session:?}");
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    assert!(
        banned.elapsed() < Duration::from_millis(100),
        "{:?}",
        banned.elapsed()
    );
    b.event("the ban", |e| {
        matches!(e, Event::RoomRemoved { .. }).then_some(())
    })
    .await;
    wait("B's viewer closed", || {
        b_window.shut_down.load(Ordering::SeqCst)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_leaving_the_room_ends_its_stream() {
    let (mut a, mut b, mut c, room) = three().await;
    c.go_live(&room, 4).await;
    let b_window = watching(&room, &c, &b).await;
    c.ok(Command::LeaveRoom { room: room.clone() }).await;
    assert!(!c.fake.panic_key_hooked(), "the hook goes with the stream");
    let reason = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert!(
        matches!(reason, WatchEnd::Ended | WatchEnd::ConnectionLost),
        "{reason:?}"
    );
    b.wait_session(&room, &c, "C's session gone", |s| s.is_none())
        .await;
    wait("B's viewer closed", || {
        b_window.shut_down.load(Ordering::SeqCst)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_window_closing_ends_the_session() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    watching(&room, &a, &b).await;
    a.fake.closed.store(true, Ordering::SeqCst);
    let reason = a
        .event("the end", |e| match e {
            Event::SessionEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, SessionEnd::SourceLost);
    assert!(!a.fake.panic_key_hooked(), "the hook goes with the stream");
    let ended = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(ended, WatchEnd::Ended);
    a.wait_session(&room, &a, "no session", |s| s.is_none())
        .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_and_going_live_again() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let first = watching(&room, &a, &b).await;
    a.ok(Command::StopLive).await;
    assert!(!a.fake.panic_key_hooked(), "the hook goes with the stream");
    let ended = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(ended, WatchEnd::Ended);
    wait("the first viewer closed", || {
        first.shut_down.load(Ordering::SeqCst)
    })
    .await;
    b.wait_session(&room, &a, "no session", |s| s.is_none())
        .await;

    // A new session on the same connections.
    a.go_live(&room, 4).await;
    let second = watching(&room, &a, &b).await;
    assert_eq!(*second.first_was_key.lock().unwrap(), Some(true));
    b.ok(Command::StopWatching).await;
    wait("the second viewer closed", || {
        second.shut_down.load(Ordering::SeqCst)
    })
    .await;
    a.wait_session(&room, &a, "no viewers", |s| {
        s.is_some_and(|s| s.viewers.is_empty())
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutting_down_while_a_guest_plays_releases_first() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::A);
    wait("B's button", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;
    let fake = a.fake.clone();
    a.stop().await;
    assert_eq!(
        fake.pad(),
        Some(PadState::NEUTRAL),
        "released before the end"
    );
    assert!(!fake.panic_key_hooked(), "the hook goes with the stream");
    let ended = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert!(
        matches!(ended, WatchEnd::Ended | WatchEnd::ConnectionLost),
        "{ended:?}"
    );
    stop_all(&mut [&mut b, &mut c]).await;
}

/// The panic key also stops a grant already on its way to the input task:
/// one the host's app sent just before the key was pressed.
#[tokio::test(flavor = "multi_thread")]
async fn after_the_panic_key_no_grant_goes_through_until_allowed() {
    use couch_engine::pipeline::host::{HostEvent, HostOptions, HostStream, Rates};
    use couch_input::{MemberId, Permissions, SeatEpoch};

    let fake = Arc::new(Fake::default());
    let (stream, mut events) = HostStream::start(
        fake.clone(),
        HostOptions {
            source: Source::Window(WINDOW),
            fps: 30,
            rates: Rates::default(),
            upload_bps: None,
            audio: couch_engine::pipeline::AudioChoice::Off,
            real_pad_ids: false,
            session: 1,
            first_epoch: SeatEpoch(0),
            rc_trace: None,
            netem_elapsed: None,
        },
    )
    .await;
    let guest = MemberId([9; 32]);
    let granted = |events: &mut mpsc::UnboundedReceiver<HostEvent>| {
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|e| matches!(e, HostEvent::Granted { .. }))
    };

    stream.panic_handle().panic();
    stream.grant(guest, Permissions::GAMEPAD);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!granted(&mut events), "refused while paused");

    stream.allow_grants();
    stream.grant(guest, Permissions::GAMEPAD);
    let deadline = Instant::now() + WAIT;
    while !granted(&mut events) {
        assert!(Instant::now() < deadline, "granted once allowed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stream.stop().await;
}
