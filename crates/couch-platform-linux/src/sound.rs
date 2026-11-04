//! A Linux host's sound (M16c, ADR 0012), through PipeWire: what every
//! program on the PC plays but this app (its chime, a stream window on the
//! same PC), or one program alone, picked by its name.
//!
//! Each program's output stream is linked into a capture stream of ours,
//! whose input ports PipeWire mixes, and which it converts to the stream's
//! format (48 kHz stereo, 32-bit float). A stream that starts later is linked
//! as it comes; our links go with our capture. A program is ours when its
//! process is this one or one of its descendants (the webview's processes
//! play the chime), by the process ids PipeWire knows for its stream.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::{Rc, Weak},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use couch_media::{AudioChunk, AudioSource, MediaError, Micros};
use pipewire as pw;
use pw::{
    properties::properties,
    spa::{
        self,
        param::{
            ParamType,
            audio::{AudioFormat, AudioInfoRaw},
        },
        pod::{Object, Pod, Value, serialize::PodSerializer},
        utils::{Direction, SpaTypes, dict::DictRef},
    },
    stream::{StreamFlags, StreamState},
    types::ObjectType,
};

/// The stream's format, which PipeWire converts to.
pub(crate) const RATE: u32 = 48_000;
pub(crate) const CHANNELS: u16 = 2;
/// How long starting, or listing what plays, may take: PipeWire answers at
/// once where it runs.
const READY_WAIT: Duration = Duration::from_secs(3);
/// Chunks waiting for the pipeline before newer ones are dropped.
const QUEUE: usize = 64;
/// How long ending a capture waits for PipeWire to take its links down.
const SETTLE_WAIT: Duration = Duration::from_millis(500);
/// How long after one capture ends the next waits to start: PipeWire 1.0
/// (Ubuntu 24.04's, in WSL) froze a program whose stream we linked again
/// within about a second of our last links to it going, its own output
/// silent too. A stream started again at once does that; a second is
/// enough there, this leaves room.
const COOLDOWN: Duration = Duration::from_millis(1500);
/// How long a start waits for the last capture to end: a stream started
/// again ends its last one only after its next read, then its settle.
const GATE_WAIT: Duration = Duration::from_secs(5);

/// One capture at a time in this process, and [`COOLDOWN`] between them: a
/// stream started again ("Stream this instead") starts its new capture
/// before the old one's thread has ended.
#[derive(Debug, Default)]
struct Gate {
    live: usize,
    last_end: Option<Instant>,
}

/// What a start must do now.
#[derive(Debug, PartialEq, Eq)]
enum Pass {
    /// Wait for the capture still running to end.
    Busy,
    /// Wait this long, the cooldown's rest.
    Cool(Duration),
    Go,
}

impl Gate {
    fn pass(&self, now: Instant) -> Pass {
        if self.live > 0 {
            return Pass::Busy;
        }
        match self
            .last_end
            .and_then(|end| COOLDOWN.checked_sub(now.saturating_duration_since(end)))
        {
            Some(rest) if !rest.is_zero() => Pass::Cool(rest),
            _ => Pass::Go,
        }
    }
}

static GATE: Mutex<Gate> = Mutex::new(Gate {
    live: 0,
    last_end: None,
});
static GATE_CHANGED: Condvar = Condvar::new();

/// How often a start waiting at the gate looks at its stream's stop.
const GATE_SLICE: Duration = Duration::from_millis(100);

/// Waits for the gate (at most [`GATE_WAIT`] for a capture still running,
/// then the cooldown), and counts a capture in; gives up, counting none,
/// once `stop` is set: a stream stopped while it waited captures nothing,
/// and holds up neither the next start nor the session's end.
fn enter(stop: &AtomicBool) -> Result<(), MediaError> {
    let deadline = Instant::now() + GATE_WAIT;
    let mut gate = GATE.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(MediaError::Closed);
        }
        let now = Instant::now();
        match gate.pass(now) {
            Pass::Busy if now < deadline => {
                gate = GATE_CHANGED
                    .wait_timeout(gate, GATE_SLICE.min(deadline - now))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            Pass::Cool(rest) => {
                drop(gate);
                std::thread::sleep(rest.min(GATE_SLICE));
                gate = GATE.lock().unwrap_or_else(PoisonError::into_inner);
            }
            // One that never ended in time is let be.
            Pass::Busy | Pass::Go => break,
        }
    }
    gate.live += 1;
    Ok(())
}

/// A capture counted in at the gate, counted out when its thread ends,
/// however it ends.
struct Counted;

impl Drop for Counted {
    fn drop(&mut self) {
        leave();
    }
}

/// A capture's thread has ended, its node and links gone.
fn leave() {
    let mut gate = GATE.lock().unwrap_or_else(PoisonError::into_inner);
    gate.live = gate.live.saturating_sub(1);
    gate.last_end = Some(Instant::now());
    GATE_CHANGED.notify_all();
}
/// A program's output stream, as PipeWire classes it.
const OUTPUT_STREAM: &str = "Stream/Output/Audio";

/// Which of what the PC plays a host sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sounds {
    /// Every program but this app.
    AllButOurs,
    /// One program's streams, by the name [`programs`] lists it under.
    Program(String),
}

/// The programs playing sound now, by name, this app left out: what a host
/// may pick from. Sorted, each once.
pub fn programs() -> Result<Vec<String>, MediaError> {
    let (done_tx, done) = mpsc::channel();
    let (quit, quit_rx) = pw::channel::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("pipewire-programs".into())
        .spawn(move || {
            let listed = list(quit_rx).map_err(|e| format!("PipeWire: {e}"));
            let _ = done_tx.send(listed);
        })
        .map_err(|e| MediaError::Backend(format!("no thread for PipeWire: {e}")))?;
    let listed = done.recv_timeout(READY_WAIT);
    let _ = quit.send(());
    let _ = thread.join();
    match listed {
        Ok(Ok(names)) => Ok(names),
        Ok(Err(e)) => Err(MediaError::Backend(e)),
        Err(_) => Err(MediaError::Backend("PipeWire didn't answer".into())),
    }
}

/// [`programs`]' side, on its own thread: the registry once through.
fn list(quit: pw::channel::Receiver<()>) -> Result<Vec<String>, pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let _quit = quit.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });
    let graph = Graph::new(Sounds::AllButOurs, String::new(), registry.clone(), None);
    let _registry = registry
        .add_listener_local()
        .global({
            let graph = graph.clone();
            move |global| graph.borrow_mut().seen(global)
        })
        .register();
    // Everything the registry holds now has come once this is answered;
    // the infos of what it bound, once a second one is.
    settle(&mainloop, &core);
    settle(&mainloop, &core);
    let graph = graph.borrow();
    let mut names: Vec<String> = graph.taken.values().flatten().cloned().collect();
    names.sort_unstable();
    names.dedup();
    Ok(names)
}

/// What this PC plays, of [`Sounds`], until dropped.
pub struct SystemSound {
    chunks: mpsc::Receiver<AudioChunk>,
    quit: pw::channel::Sender<()>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl SystemSound {
    /// Starts capturing; fails at once where PipeWire doesn't run. Silent
    /// while nothing it takes plays. Waits first for this process's last
    /// capture to end, and the cooldown; `MediaError::Closed` if `stop`
    /// (the stream's) is set meanwhile.
    pub fn start(sounds: Sounds, stop: &AtomicBool) -> Result<Self, MediaError> {
        enter(stop)?;
        let counted = Counted;
        let (chunks_tx, chunks) = mpsc::sync_channel(QUEUE);
        let (ready_tx, ready) = mpsc::channel::<Result<(), String>>();
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let error = Arc::new(Mutex::new(None));
        let thread_error = error.clone();
        let thread = std::thread::Builder::new()
            .name("pipewire-sound".into())
            .spawn(move || {
                let _counted = counted;
                if let Err(e) = capture(sounds, &chunks_tx, &ready_tx, &thread_error, quit_rx) {
                    let e = format!("PipeWire: {e}");
                    // Before it was ready, starting fails; after, the next
                    // read says why the sound stopped.
                    if ready_tx.send(Err(e.clone())).is_err() {
                        *thread_error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
                    }
                }
            })
            // A thread never made dropped the closure: counted out.
            .map_err(|e| MediaError::Backend(format!("no thread for PipeWire: {e}")))?;
        let started = ready.recv_timeout(READY_WAIT);
        let sound = Self {
            chunks,
            quit,
            error,
            thread: Some(thread),
        };
        match started {
            Ok(Ok(())) => Ok(sound),
            Ok(Err(e)) => Err(MediaError::Backend(e)),
            Err(_) => Err(MediaError::Backend("PipeWire didn't answer".into())),
        }
    }
}

impl AudioSource for SystemSound {
    fn next_chunk(&mut self, timeout: Duration) -> Result<Option<AudioChunk>, MediaError> {
        match self.chunks.recv_timeout(timeout) {
            Ok(chunk) => Ok(Some(chunk)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self
                .error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .map_or(MediaError::Closed, MediaError::Backend)),
        }
    }
}

impl Drop for SystemSound {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the stream's callbacks keep.
struct Sound {
    chunks: mpsc::SyncSender<AudioChunk>,
    error: Arc<Mutex<Option<String>>>,
    mainloop: pw::main_loop::MainLoopRc,
}

/// PipeWire's side, on its own thread, until `quit` or the stream fails.
fn capture(
    sounds: Sounds,
    chunks: &mpsc::SyncSender<AudioChunk>,
    ready: &mpsc::Sender<Result<(), String>>,
    error: &Arc<Mutex<Option<String>>>,
    quit: pw::channel::Receiver<()>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let _quit = quit.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });
    // Named so our own node and its ports are found in the registry.
    static CAPTURES: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "couch-coapp-sound-{}-{}",
        std::process::id(),
        CAPTURES.fetch_add(1, Ordering::Relaxed)
    );
    let graph = Graph::new(sounds, name.clone(), registry.clone(), Some(core.clone()));
    let _registry = registry
        .add_listener_local()
        .global({
            let graph = graph.clone();
            move |global| graph.borrow_mut().seen(global)
        })
        .global_remove({
            let graph = graph.clone();
            move |id| graph.borrow_mut().gone(id)
        })
        .register();
    let stream = pw::stream::StreamBox::new(
        &core,
        "couch-coapp-sound",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Game",
            *pw::keys::NODE_NAME => name.as_str(),
            // Linked by us alone, to what it takes.
            *pw::keys::NODE_AUTOCONNECT => "false",
            // Run even while nothing it takes plays: silence comes all
            // along, as Windows' loopback gives it.
            *pw::keys::NODE_ALWAYS_PROCESS => "true",
            // 10 ms, the codec's frame.
            *pw::keys::NODE_LATENCY => "480/48000",
        },
    )?;
    let _listener = stream
        .add_local_listener_with_user_data(Sound {
            chunks: chunks.clone(),
            error: error.clone(),
            mainloop: mainloop.clone(),
        })
        .state_changed(|_, sound, _, state| {
            if let StreamState::Error(e) = state {
                *sound.error.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(format!("the sound capture failed: {e}"));
                sound.mainloop.quit();
            }
        })
        .process(|stream, sound| {
            if let Some(mut buffer) = stream.dequeue_buffer()
                && let Some(data) = buffer.datas_mut().first_mut()
            {
                let chunk = data.chunk();
                let (offset, size) = (chunk.offset() as usize, chunk.size() as usize);
                if let Some(bytes) = data.data().and_then(|b| b.get(offset..offset + size)) {
                    let samples = samples(bytes);
                    let frames = (samples.len() / usize::from(CHANNELS)) as u64;
                    // Stamped with its first sample's time.
                    let now = crate::clock::now().0;
                    let chunk = AudioChunk {
                        samples,
                        channels: CHANNELS,
                        sample_rate: RATE,
                        captured_at: Micros(
                            now.saturating_sub(frames * 1_000_000 / u64::from(RATE)),
                        ),
                    };
                    // A pipeline that fell behind loses the newest, as a full
                    // queue anywhere does.
                    let _ = sound.chunks.try_send(chunk);
                }
            }
        })
        .register()?;
    let params = format();
    let mut params = [Pod::from_bytes(&params).ok_or(pw::Error::CreationFailed)?];
    // On this thread's main loop, not PipeWire's realtime one: the callbacks
    // share their state, and 10 ms quanta need no more.
    stream.connect(
        Direction::Input,
        None,
        StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    let _ = ready.send(Ok(()));
    mainloop.run();
    // Our node and links go, and PipeWire says so, before the next capture
    // may start ([`COOLDOWN`] after this).
    graph.borrow_mut().closing = true;
    let _ = stream.disconnect();
    settle(&mainloop, &core);
    Ok(())
}

/// Runs the loop until PipeWire has done all we asked, or for
/// [`SETTLE_WAIT`] at most.
fn settle(mainloop: &pw::main_loop::MainLoopRc, core: &pw::core::CoreRc) {
    let Ok(pending) = core.sync(0) else { return };
    let _done = core
        .add_listener_local()
        .done({
            let mainloop = mainloop.clone();
            move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    mainloop.quit();
                }
            }
        })
        .register();
    let timer = mainloop.loop_().add_timer({
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });
    let _ = timer.update_timer(Some(SETTLE_WAIT), None);
    mainloop.run();
}

/// What a client or a stream says of its program: its process ids (the one
/// it declares, and the one the kernel gave PipeWire for its connection)
/// and its name.
#[derive(Debug, Default, Clone)]
struct Facts {
    pids: Vec<u32>,
    name: Option<String>,
}

impl Facts {
    fn from(props: &DictRef) -> Self {
        Self {
            pids: [*pw::keys::APP_PROCESS_ID, *pw::keys::SEC_PID]
                .into_iter()
                .filter_map(|key| number(props, key))
                .collect(),
            name: program_name(props),
        }
    }
}

/// A program's output stream, as far as we know it.
struct OutputStream {
    client: Option<u32>,
    facts: Facts,
    /// It plays again what others play: a loopback's or a filter's output
    /// (`node.link-group`, `stream.is-loopback`).
    replays: bool,
    /// Its info has come: its own props, not the registry's snapshot alone.
    known: bool,
}

/// A proxy bound to learn an object's info, with its listener (dropped
/// first).
struct Watched<L, P> {
    _listener: L,
    _proxy: P,
}

/// Which side of our stereo input a channel goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// What the registry shows of the graph, and the links we made in it.
///
/// Who a stream is decides on its info and its client's, not the registry's
/// snapshot alone: a PulseAudio client (WebKit plays the chime through one)
/// shows there only pipewire-pulse's own process id, its own only in its
/// info. A stream is taken once its info has come, and given up again if
/// later facts make it ours, or a replay.
struct Graph {
    sounds: Sounds,
    /// This process: it, and its descendants, are ours.
    us: u32,
    /// Our capture's node, by its name, once seen.
    our_name: String,
    our_node: Option<u32>,
    /// Our node's input ports, by id.
    inputs: HashMap<u32, Side>,
    clients: HashMap<u32, Facts>,
    streams: HashMap<u32, OutputStream>,
    watched_clients: HashMap<u32, Watched<pw::client::ClientListener, pw::client::Client>>,
    watched_streams: HashMap<u32, Watched<pw::node::NodeListener, pw::node::Node>>,
    /// Clients with a sink of their own (loopbacks, filters, effects): their
    /// streams play again what others play into it.
    routers: HashSet<u32>,
    /// The output streams we take, by node id, with their programs' names.
    taken: HashMap<u32, Option<String>>,
    /// Every output stream's output ports: id → (node, channel).
    outputs: HashMap<u32, (u32, String)>,
    /// Ours, by (output port, input port).
    links: HashMap<(u32, u32), pw::link::Link>,
    /// The capture is ending: no new links.
    closing: bool,
    /// To bind what we watch, and to link (none when only listing).
    registry: pw::registry::RegistryRc,
    core: Option<pw::core::CoreRc>,
    this: Weak<RefCell<Graph>>,
}

impl Graph {
    fn new(
        sounds: Sounds,
        our_name: String,
        registry: pw::registry::RegistryRc,
        core: Option<pw::core::CoreRc>,
    ) -> Rc<RefCell<Self>> {
        Rc::new_cyclic(|this| {
            RefCell::new(Self {
                sounds,
                us: std::process::id(),
                our_name,
                our_node: None,
                inputs: HashMap::new(),
                clients: HashMap::new(),
                streams: HashMap::new(),
                watched_clients: HashMap::new(),
                watched_streams: HashMap::new(),
                routers: HashSet::new(),
                taken: HashMap::new(),
                outputs: HashMap::new(),
                links: HashMap::new(),
                closing: false,
                registry,
                core,
                this: this.clone(),
            })
        })
    }

    /// A new object in the registry.
    fn seen(&mut self, global: &pw::registry::GlobalObject<&DictRef>) {
        let Some(props) = global.props else { return };
        let id = global.id;
        match global.type_ {
            ObjectType::Client => {
                self.clients.insert(id, Facts::from(props));
                if let Ok(proxy) = self.registry.bind::<pw::client::Client, _>(global) {
                    let this = self.this.clone();
                    let listener = proxy
                        .add_listener_local()
                        .info(move |info| {
                            if let (Some(graph), Some(props)) = (this.upgrade(), info.props()) {
                                graph.borrow_mut().client_info(id, props);
                            }
                        })
                        .register();
                    self.watched_clients.insert(
                        id,
                        Watched {
                            _listener: listener,
                            _proxy: proxy,
                        },
                    );
                }
            }
            ObjectType::Node => {
                let class = props.get(*pw::keys::MEDIA_CLASS);
                let client = number(props, *pw::keys::CLIENT_ID);
                if !self.our_name.is_empty()
                    && props.get(*pw::keys::NODE_NAME) == Some(self.our_name.as_str())
                {
                    self.our_node = Some(id);
                } else if class == Some(OUTPUT_STREAM) {
                    self.streams.insert(
                        id,
                        OutputStream {
                            client,
                            facts: Facts::from(props),
                            replays: replays(props),
                            known: false,
                        },
                    );
                    if let Ok(proxy) = self.registry.bind::<pw::node::Node, _>(global) {
                        let this = self.this.clone();
                        let listener = proxy
                            .add_listener_local()
                            .info(move |info| {
                                if let (Some(graph), Some(props)) = (this.upgrade(), info.props()) {
                                    graph.borrow_mut().stream_info(id, props);
                                }
                            })
                            .register();
                        self.watched_streams.insert(
                            id,
                            Watched {
                                _listener: listener,
                                _proxy: proxy,
                            },
                        );
                    }
                } else if class.is_some_and(|c| c.starts_with("Audio/Sink"))
                    && let Some(client) = client
                    && self.routers.insert(client)
                {
                    self.decide_client(client);
                }
            }
            ObjectType::Port => {
                let (Some(node), Some(channel)) = (
                    number(props, *pw::keys::NODE_ID),
                    props.get(*pw::keys::AUDIO_CHANNEL),
                ) else {
                    return;
                };
                if props.get(*pw::keys::PORT_MONITOR) == Some("true") {
                    return;
                }
                match props.get(*pw::keys::PORT_DIRECTION) {
                    Some("in") if Some(node) == self.our_node => {
                        let side = match channel {
                            "FL" => Side::Left,
                            "FR" => Side::Right,
                            _ => return,
                        };
                        self.inputs.insert(id, side);
                    }
                    Some("out") => {
                        self.outputs.insert(id, (node, channel.to_string()));
                    }
                    _ => return,
                }
                self.link();
            }
            _ => {}
        }
    }

    /// A client's own props: its streams decided again.
    fn client_info(&mut self, id: u32, props: &DictRef) {
        self.clients.insert(id, Facts::from(props));
        self.decide_client(id);
    }

    /// A stream's own props: decided now.
    fn stream_info(&mut self, id: u32, props: &DictRef) {
        let Some(stream) = self.streams.get_mut(&id) else {
            return;
        };
        stream.facts = Facts::from(props);
        stream.replays |= replays(props);
        if let Some(client) = number(props, *pw::keys::CLIENT_ID) {
            stream.client = Some(client);
        }
        stream.known = true;
        self.decide(id);
    }

    fn decide_client(&mut self, client: u32) {
        let streams: Vec<u32> = self
            .streams
            .iter()
            .filter(|(_, s)| s.client == Some(client))
            .map(|(&id, _)| id)
            .collect();
        for id in streams {
            self.decide(id);
        }
    }

    /// Takes the stream `id`, or gives it up, by what we know of it now.
    fn decide(&mut self, id: u32) {
        let Some(stream) = self.streams.get(&id) else {
            return;
        };
        let client = stream.client.and_then(|c| self.clients.get(&c));
        let ours = stream
            .facts
            .pids
            .iter()
            .chain(client.map(|c| c.pids.as_slice()).unwrap_or_default())
            .any(|&pid| is_ours(pid, self.us));
        let replay = stream.replays || stream.client.is_some_and(|c| self.routers.contains(&c));
        let name = stream
            .facts
            .name
            .clone()
            .or_else(|| client.and_then(|c| c.name.clone()));
        let takes = stream.known
            && !ours
            && !replay
            && match &self.sounds {
                Sounds::AllButOurs => true,
                Sounds::Program(program) => name.as_ref() == Some(program),
            };
        if takes {
            self.taken.insert(id, name);
            self.link();
        } else if self.taken.remove(&id).is_some() {
            self.unlink(id);
        }
    }

    /// An object gone, and our links with it.
    fn gone(&mut self, id: u32) {
        self.clients.remove(&id);
        self.watched_clients.remove(&id);
        self.streams.remove(&id);
        self.watched_streams.remove(&id);
        self.routers.remove(&id);
        self.taken.remove(&id);
        self.outputs.remove(&id);
        self.inputs.remove(&id);
        if self.our_node == Some(id) {
            self.our_node = None;
        }
        // The server took them down with the port.
        self.links
            .retain(|&(output, input), _| output != id && input != id);
    }

    /// Takes our links to the stream `node` down.
    fn unlink(&mut self, node: u32) {
        let ports: HashSet<u32> = self
            .outputs
            .iter()
            .filter(|(_, (n, _))| *n == node)
            .map(|(&port, _)| port)
            .collect();
        let gone: Vec<(u32, u32)> = self
            .links
            .keys()
            .filter(|(output, _)| ports.contains(output))
            .copied()
            .collect();
        // Dropping our proxy takes an unlingering link down.
        for key in gone {
            self.links.remove(&key);
        }
    }

    /// Links each output port of what we take to our inputs on its side.
    fn link(&mut self) {
        let (Some(core), Some(our_node)) = (&self.core, self.our_node.filter(|_| !self.closing))
        else {
            return;
        };
        for (&output, (node, channel)) in &self.outputs {
            if !self.taken.contains_key(node) {
                continue;
            }
            for (&input, &side) in &self.inputs {
                if !sides(channel).contains(&side) || self.links.contains_key(&(output, input)) {
                    continue;
                }
                let made = core.create_object::<pw::link::Link>(
                    "link-factory",
                    &properties! {
                        *pw::keys::LINK_OUTPUT_NODE => node.to_string(),
                        *pw::keys::LINK_OUTPUT_PORT => output.to_string(),
                        *pw::keys::LINK_INPUT_NODE => our_node.to_string(),
                        *pw::keys::LINK_INPUT_PORT => input.to_string(),
                        // Gone with us.
                        *pw::keys::OBJECT_LINGER => "false",
                    },
                );
                match made {
                    Ok(link) => {
                        self.links.insert((output, input), link);
                    }
                    // Tried again at the graph's next change.
                    Err(e) => tracing::debug!("PipeWire: no link for a program's sound: {e}"),
                }
            }
        }
    }
}

/// Whether a stream plays again what others play: a loopback's or a filter
/// chain's output (the Steam Deck's speaker filter is one).
fn replays(props: &DictRef) -> bool {
    props.get("node.link-group").is_some() || props.get("stream.is-loopback") == Some("true")
}

/// Which of our two inputs a program's channel goes to: stereo as it is,
/// sides and rears with their fronts, mono and centre to both; the
/// low-frequency channel left out.
fn sides(channel: &str) -> &'static [Side] {
    match channel {
        "FL" | "SL" | "RL" | "FLC" | "AUX0" => &[Side::Left],
        "FR" | "SR" | "RR" | "FRC" | "AUX1" => &[Side::Right],
        "MONO" | "FC" | "RC" => &[Side::Left, Side::Right],
        _ => &[],
    }
}

/// A program's name, as PipeWire has it: the application's, or its binary's.
fn program_name(props: &DictRef) -> Option<String> {
    [*pw::keys::APP_NAME, *pw::keys::APP_PROCESS_BINARY]
        .into_iter()
        .filter_map(|key| props.get(key))
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map(str::to_string)
}

fn number(props: &DictRef, key: &str) -> Option<u32> {
    props.get(key)?.trim().parse().ok()
}

/// Whether `pid` is `us` or one of its descendants, by `/proc`.
fn is_ours(pid: u32, us: u32) -> bool {
    let mut pid = pid;
    // A tree deeper than this is no one's.
    for _ in 0..64 {
        if pid == us {
            return true;
        }
        match parent(pid) {
            Some(parent) if parent > 1 => pid = parent,
            _ => return false,
        }
    }
    false
}

/// A process's parent: the field after its state in `/proc/<pid>/stat`, past
/// the name in parentheses (which may hold anything).
fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// 48 kHz stereo, 32-bit float: PipeWire converts whatever plays to it.
pub(crate) fn format() -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(u32::from(CHANNELS));
    let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let object = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .unwrap_or_default()
}

/// Little-endian 32-bit floats; a trailing partial sample is left out.
fn samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    /// A capture, its stream never stopped.
    fn start(sounds: Sounds) -> SystemSound {
        SystemSound::start(sounds, &AtomicBool::new(false)).expect("PipeWire runs")
    }

    #[test]
    fn floats_are_read_little_endian() {
        let mut bytes = Vec::new();
        for v in [0.5f32, -1.0, 0.25] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.push(7);
        assert_eq!(samples(&bytes), [0.5, -1.0, 0.25]);
    }

    #[test]
    fn the_format_serializes() {
        assert!(Pod::from_bytes(&format()).is_some());
    }

    #[test]
    fn the_gate_waits_for_the_last_capture_then_the_cooldown() {
        let now = Instant::now();
        let running = Gate {
            live: 1,
            last_end: None,
        };
        assert_eq!(running.pass(now), Pass::Busy);
        assert_eq!(Gate::default().pass(now), Pass::Go);
        let ended = Gate {
            live: 0,
            last_end: Some(now),
        };
        assert_eq!(ended.pass(now), Pass::Cool(COOLDOWN));
        let half = Duration::from_millis(500);
        assert_eq!(ended.pass(now + half), Pass::Cool(COOLDOWN - half));
        assert_eq!(ended.pass(now + COOLDOWN), Pass::Go);
    }

    /// A start waiting at the gate for a capture still running gives up
    /// once its stream stops, and counts nothing in.
    #[test]
    fn a_stopped_start_gives_up_at_the_gate() {
        GATE.lock().unwrap().live += 1;
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| enter(&stop));
            std::thread::sleep(Duration::from_millis(250));
            assert!(!waiting.is_finished(), "waits while a capture runs");
            let stopped = Instant::now();
            stop.store(true, Ordering::SeqCst);
            assert!(matches!(waiting.join().unwrap(), Err(MediaError::Closed)));
            assert!(stopped.elapsed() < Duration::from_millis(500));
        });
        let mut gate = GATE.lock().unwrap();
        assert_eq!(gate.live, 1, "nothing counted in");
        gate.live -= 1;
    }

    #[test]
    fn channels_go_to_their_sides() {
        assert_eq!(sides("FL"), [Side::Left]);
        assert_eq!(sides("RR"), [Side::Right]);
        assert_eq!(sides("MONO"), [Side::Left, Side::Right]);
        assert_eq!(sides("FC"), [Side::Left, Side::Right]);
        assert!(sides("LFE").is_empty());
    }

    #[test]
    fn our_children_are_ours_and_others_are_not() {
        let us = std::process::id();
        assert!(is_ours(us, us));
        let mut child = Command::new("sleep").arg("5").spawn().expect("sleep runs");
        assert!(is_ours(child.id(), us));
        let _ = child.kill();
        let _ = child.wait();
        assert!(!is_ours(1, us));
        assert!(!is_ours(parent(us).expect("we have a parent"), us));
        assert!(!is_ours(u32::MAX, us));
    }

    /// A WAV file of a sine, 48 kHz stereo, `secs` long, in the temporary
    /// folder.
    fn sine_file(frequency: u32, secs: u32) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("couch-test-sine-{frequency}.wav"));
        let frames = RATE * secs;
        let data = frames * 4;
        let mut wav = Vec::with_capacity(44 + data as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&CHANNELS.to_le_bytes());
        wav.extend_from_slice(&RATE.to_le_bytes());
        wav.extend_from_slice(&(RATE * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data.to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / RATE as f32;
            let v = (0.5 * (t * frequency as f32 * std::f32::consts::TAU).sin() * 32767.0) as i16;
            wav.extend_from_slice(&v.to_le_bytes());
            wav.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(&path, wav).expect("the temporary folder takes it");
        path
    }

    /// The loudest sample heard within `secs`; and that sound, silence
    /// included, came all along, as Windows' loopback gives it.
    fn peak(sound: &mut SystemSound, secs: u32) -> f32 {
        let mut peak = 0.0f32;
        let mut frames = 0;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(secs.into()) {
            if let Some(chunk) = sound.next_chunk(Duration::from_millis(100)).unwrap() {
                assert_eq!((chunk.sample_rate, chunk.channels), (RATE, CHANNELS));
                peak = chunk.samples.iter().fold(peak, |p, s| p.max(s.abs()));
                frames += chunk.samples.len() / usize::from(CHANNELS);
            }
        }
        let heard = frames as f32 / RATE as f32;
        assert!(
            heard > secs as f32 - 0.5,
            "only {heard:.2} s of sound in {secs} s"
        );
        peak
    }

    /// The loudest sample the test output (`linux-checks`' null output,
    /// `couch-test-sink`) plays within a second, from its monitor: what
    /// the PC's own listener would hear.
    fn output_peak() -> f32 {
        let file = std::env::temp_dir().join("couch-test-output.wav");
        let _ = Command::new("timeout")
            .args(["1", "pw-record", "--target", "couch-test-sink"])
            .args(["-P", "{ stream.capture.sink = true }"])
            .args(["--format", "s16", "--channels", "2"])
            .arg(&file)
            .status();
        let wav = std::fs::read(&file).unwrap_or_default();
        let loudest = wav
            .get(44..)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| i16::from_le_bytes(b).unsigned_abs())
            .max()
            .unwrap_or(0);
        f32::from(loudest) / 32768.0
    }

    /// Where PipeWire runs, with something playing (`linux-checks` plays a
    /// sine into a null output): chunks come in the stream's format, and
    /// carry it.
    #[test]
    #[ignore = "needs a running PipeWire, with something playing"]
    fn what_plays_is_captured() {
        let mut sound = start(Sounds::AllButOurs);
        let heard = peak(&mut sound, 3);
        assert!(heard > 0.1, "nothing heard within 3 s (peak {heard})");
    }

    /// Where PipeWire runs, with `linux-checks`' null output and nothing
    /// else playing: a sine of our own (our child) is left out, silence
    /// coming meanwhile; another program's is heard, and listed while ours
    /// isn't; one program picked by name is heard alone, in a capture
    /// started as soon as the last ended; and the programs' own sound still
    /// reaches the output. The sines play through `pw-play`.
    #[test]
    #[ignore = "needs a running PipeWire with linux-checks' null output, nothing else playing, and pw-play"]
    fn ours_is_left_out_and_one_program_is_taken() {
        let ours = sine_file(440, 30);
        let theirs = sine_file(660, 30);
        let mut own = Command::new("pw-play")
            .args(["-P", "{ application.name = couch-test-ours }"])
            .arg(&ours)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("pw-play runs");
        std::thread::sleep(Duration::from_millis(500));
        let mut sound = start(Sounds::AllButOurs);
        let heard = peak(&mut sound, 2);
        drop(sound);

        // Another program's: started by a shell that leaves, so its parent
        // is no longer us.
        let other = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "setsid pw-play -P '{{ application.name = couch-test-theirs }}' {} \
                 >/dev/null 2>&1 & echo $!",
                theirs.display()
            ))
            .output()
            .expect("sh runs");
        let theirs: u32 = String::from_utf8_lossy(&other.stdout)
            .trim()
            .parse()
            .expect("a pid");
        // A loopback, which plays again what it records: not a program.
        let loopback = Command::new("sh")
            .args(["-c", "setsid pw-loopback >/dev/null 2>&1 & echo $!"])
            .output()
            .expect("sh runs");
        let loopback = String::from_utf8_lossy(&loopback.stdout).trim().to_string();
        std::thread::sleep(Duration::from_millis(500));
        let listed = programs().expect("PipeWire lists");
        let _ = Command::new("kill").arg(&loopback).status();
        let mut sound = start(Sounds::AllButOurs);
        let all = peak(&mut sound, 2);
        // Started again as the pipeline does: the next capture starts before
        // the last one ends.
        let next = std::thread::spawn(|| start(Sounds::Program("couch-test-theirs".into())));
        std::thread::sleep(Duration::from_millis(100));
        drop(sound);
        let mut sound = next.join().unwrap();
        let one = peak(&mut sound, 2);
        drop(sound);
        let mut sound = start(Sounds::Program("nobody".into()));
        let none = peak(&mut sound, 2);
        drop(sound);
        let output = output_peak();
        let _ = Command::new("kill").arg(theirs.to_string()).status();
        let _ = own.kill();
        let _ = own.wait();
        assert!(heard < 0.01, "our own sound was heard (peak {heard})");
        assert!(
            listed.iter().any(|n| n == "couch-test-theirs")
                && !listed.iter().any(|n| n == "couch-test-ours")
                && !listed.iter().any(|n| n.contains("loopback")),
            "listed {listed:?}"
        );
        assert!(all > 0.1, "another program wasn't heard (peak {all})");
        assert!(one > 0.1, "the program picked wasn't heard (peak {one})");
        assert!(none < 0.01, "a program not playing was heard (peak {none})");
        assert!(
            output > 0.5,
            "the programs' own sound stopped (peak {output})"
        );
    }

    /// Where PipeWire runs, with `linux-checks`' null output and nothing
    /// else playing: a program that ends its stream and opens a new one at
    /// once, while we capture (a game changing scenes), is heard again,
    /// and its own sound still reaches the output: our links to its old
    /// stream went with it, and the new one is linked at once.
    #[test]
    #[ignore = "needs a running PipeWire with linux-checks' null output, nothing else playing, and pw-play"]
    fn a_program_opening_its_stream_again_is_heard_again() {
        let file = sine_file(550, 3);
        let mut sound = start(Sounds::AllButOurs);
        let other = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "setsid sh -c 'pw-play {0}; pw-play {0}' >/dev/null 2>&1 & echo $!",
                file.display()
            ))
            .output()
            .expect("sh runs");
        let theirs = String::from_utf8_lossy(&other.stdout).trim().to_string();
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        let first = peak(&mut sound, 1);
        // The second stream plays from about 3 s to 6 s; read on meanwhile,
        // so nothing older waits in the queue.
        while started.elapsed() < Duration::from_millis(3500) {
            let _ = sound.next_chunk(Duration::from_millis(50)).unwrap();
        }
        let second = peak(&mut sound, 1);
        let output = output_peak();
        drop(sound);
        let _ = Command::new("pkill").args(["-s", &theirs]).status();
        assert!(first > 0.1, "the first stream wasn't heard (peak {first})");
        assert!(
            second > 0.1,
            "the stream opened again wasn't heard (peak {second})"
        );
        assert!(
            output > 0.3,
            "the program's own sound stopped (peak {output})"
        );
    }

    /// As above, through PipeWire's PulseAudio server (WebKit plays the
    /// chime as a PulseAudio client): our own `paplay` is left out, though
    /// the registry shows only pipewire-pulse's process for it; another
    /// program's is heard. `PULSE_SERVER` must name pipewire-pulse (WSLg
    /// points it at its own).
    #[test]
    #[ignore = "needs PipeWire with pipewire-pulse and linux-checks' null output, nothing else playing, and paplay"]
    fn a_pulse_client_of_ours_is_left_out() {
        let file = sine_file(440, 30);
        let mut own = Command::new("paplay")
            .args(["--client-name", "couch-test-pulse-ours"])
            .arg(&file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("paplay runs");
        std::thread::sleep(Duration::from_millis(500));
        let mut sound = start(Sounds::AllButOurs);
        let ours = peak(&mut sound, 2);
        let listed = programs().expect("PipeWire lists");
        drop(sound);
        let other = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "setsid paplay --client-name couch-test-pulse-theirs {} >/dev/null 2>&1 & echo $!",
                file.display()
            ))
            .output()
            .expect("sh runs");
        let theirs = String::from_utf8_lossy(&other.stdout).trim().to_string();
        std::thread::sleep(Duration::from_millis(500));
        let mut sound = start(Sounds::AllButOurs);
        let all = peak(&mut sound, 2);
        drop(sound);
        let _ = Command::new("kill").arg(&theirs).status();
        let _ = own.kill();
        let _ = own.wait();
        assert!(
            ours < 0.01,
            "our own PulseAudio sound was heard (peak {ours})"
        );
        assert!(
            !listed.iter().any(|n| n == "couch-test-pulse-ours"),
            "listed {listed:?}"
        );
        assert!(
            all > 0.1,
            "another PulseAudio program wasn't heard (peak {all})"
        );
    }
}
