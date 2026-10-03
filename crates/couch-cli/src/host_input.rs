//! `couch-cli host`'s input side: grants seat 0 to the connected viewer,
//! runs its input through the seat router, and injects what the router lets
//! through. In the app (M5) the host chooses who gets the seat; here the one
//! viewer gets it on connecting.

use std::{
    sync::{
        Arc,
        atomic::{AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
};

use couch_input::{
    DEV_PAD_IDS, KbmInjector, MemberId, Permissions, Rumble, SeatId, VirtualPad, VirtualPadFactory,
    router::{self, Effect, SeatRouter, SeatStatus},
};
use couch_net::{
    Connection,
    control::ControlSender,
    datagrams::Datagrams,
    input::{InputReceiver, send_rumble},
};
use couch_platform_win::{
    foreground::{WindowId, is_foreground, pointer_over},
    sendinput::SendInputInjector,
    vigem::VigemPadFactory,
};
use couch_proto::{
    control::{ControlMessage, SeatAssigned, SeatReleased},
    input::RumbleDatagram,
};
use tokio::{sync::watch, task::JoinHandle};

const SEAT: SeatId = SeatId(0);
/// How often the dead-man timer, the foreground check and rumble run.
const TICK: Duration = Duration::from_millis(10);
/// A running rumble goes out again this often.
const RUMBLE_REPEAT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct Options {
    pub permissions: Permissions,
    /// Plug the pad with a real Xbox 360 controller's ids instead of the
    /// development ids a viewer on this machine skips.
    pub real_pad_ids: bool,
    /// The captured window, once capture has started (0 before, and always
    /// for a monitor or the test pattern). Keyboard and mouse go in only
    /// while it is in front, and clicks only with the pointer over it.
    pub game_window: Arc<AtomicIsize>,
    /// Whether the source is a window at all, for a warning.
    pub window_source: bool,
}

/// Starts routing this viewer's input. Ends, releasing everything, when the
/// connection closes or `shutdown` turns true.
pub fn spawn(
    conn: Connection,
    datagrams: &Datagrams,
    control: ControlSender,
    options: Options,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let inputs = InputReceiver::start(conn.clone(), datagrams);
    tokio::spawn(async move {
        run(conn, inputs, control, options, shutdown).await;
    })
}

/// The captured window as the capture thread publishes it; 0 until then.
fn game_window(window: &AtomicIsize) -> Option<WindowId> {
    match window.load(Ordering::Acquire) {
        0 => None,
        id => Some(WindowId(id)),
    }
}

/// Updates the router's view of where keyboard and mouse would land. Called
/// right before every input, not just on the tick: a window that just took
/// the foreground must not get a key meant for the game.
fn refresh_focus(router: &mut SeatRouter, window: &AtomicIsize) -> Vec<Effect> {
    let game = game_window(window);
    router.set_pointer_inside(game.is_some_and(pointer_over));
    router.set_foreground(game.is_some_and(is_foreground))
}

fn plug_pad(real_ids: bool) -> Option<Box<dyn VirtualPad>> {
    let ids = (!real_ids).then_some(DEV_PAD_IDS);
    match VigemPadFactory::connect(ids).and_then(|f| f.plug()) {
        Ok(pad) => {
            println!("virtual Xbox 360 pad plugged in");
            Some(pad)
        }
        Err(e) => {
            println!("no virtual pad ({e}); gamepad input will be dropped");
            None
        }
    }
}

async fn run(
    conn: Connection,
    mut inputs: InputReceiver,
    mut control: ControlSender,
    options: Options,
    mut shutdown: watch::Receiver<bool>,
) {
    let real_ids = options.real_pad_ids;
    let mut pads: Vec<Box<dyn VirtualPad>> =
        tokio::task::spawn_blocking(move || plug_pad(real_ids))
            .await
            .ok()
            .flatten()
            .into_iter()
            .collect();
    let mut kbm = SendInputInjector::new();
    let member = MemberId(*conn.remote_id().as_bytes());
    let mut router = SeatRouter::new(1);
    let started = Instant::now();
    let effects = router.grant(SEAT, member, options.permissions, started);
    deliver(&effects, &mut kbm, &mut pads, &mut control).await;
    if options
        .permissions
        .intersects(Permissions::KEYBOARD | Permissions::MOUSE)
        && !options.window_source
    {
        println!("keyboard and mouse stay off: they need --source window");
    }

    let mut status = router.seat(SEAT).map(|s| s.2);
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The game's current rumble, and what last went to the viewer, when.
    let mut rumble = Rumble::default();
    let mut rumble_sent = (Rumble::default(), started);
    loop {
        let effects = tokio::select! {
            input = inputs.inputs.recv() => match input {
                Some(input) => {
                    let mut effects = refresh_focus(&mut router, &options.game_window);
                    effects.extend(router.handle(member, input, Instant::now()));
                    effects
                }
                None => break,
            },
            _ = shutdown.changed() => break,
            _ = tick.tick() => {
                let now = Instant::now();
                let mut effects = router.tick(now);
                effects.extend(refresh_focus(&mut router, &options.game_window));
                for pad in &mut pads {
                    if let Some(r) = pad.take_rumble() {
                        println!(
                            "{:8.3}s  rumble from the game: low {} high {}",
                            started.elapsed().as_secs_f64(),
                            r.low_frequency,
                            r.high_frequency
                        );
                        rumble = r;
                    }
                }
                // Rumble goes only to the seat's holder. A running rumble is
                // repeated, so a lost "stop" can't leave the controller
                // shaking: the viewer ends each one after a second.
                let held = router.seat(SEAT).is_some_and(|s| s.1.is_some());
                let wanted = if held { rumble } else { Rumble::default() };
                let repeat = wanted != Rumble::default() && now.duration_since(rumble_sent.1) >= RUMBLE_REPEAT;
                if wanted != rumble_sent.0 || repeat {
                    send_rumble(&conn, RumbleDatagram {
                        seat: SEAT,
                        low: wanted.low_frequency,
                        high: wanted.high_frequency,
                    });
                    rumble_sent = (wanted, now);
                }
                effects
            }
        };
        deliver(&effects, &mut kbm, &mut pads, &mut control).await;
        let now_status = router.seat(SEAT).map(|s| s.2);
        if now_status != status {
            status = now_status;
            let what = match status {
                Some(SeatStatus::Active) => "active",
                Some(SeatStatus::Suspended) => "suspended, everything released",
                Some(SeatStatus::Host) => "back with the host",
                Some(SeatStatus::Granted) | None => "waiting",
            };
            println!("{:8.3}s  seat 0 {what}", started.elapsed().as_secs_f64());
        }
    }
    let effects = router.revoke_all();
    if let Err(e) = router::apply(&effects, &mut kbm, &mut pads) {
        println!("releasing input failed: {e}");
    }
    let why = if *shutdown.borrow() {
        "host stopping"
    } else {
        "viewer gone"
    };
    println!(
        "{:8.3}s  {why}: input released",
        started.elapsed().as_secs_f64()
    );
}

/// Injects the effects and tells the viewer about seat changes.
async fn deliver(
    effects: &[Effect],
    kbm: &mut dyn KbmInjector,
    pads: &mut [Box<dyn VirtualPad>],
    control: &mut ControlSender,
) {
    if let Err(e) = router::apply(effects, kbm, pads) {
        tracing::warn!("injection failed: {e}");
    }
    for effect in effects {
        let message = match *effect {
            Effect::Granted {
                seat,
                epoch,
                permissions,
                ..
            } => ControlMessage::SeatAssigned(SeatAssigned {
                seat,
                seat_epoch: epoch,
                permissions,
            }),
            Effect::Revoked { seat, epoch, .. } => ControlMessage::SeatReleased(SeatReleased {
                seat,
                seat_epoch: epoch,
            }),
            _ => continue,
        };
        if let Err(e) = control.send(&message).await {
            tracing::debug!("seat notice not sent: {e}");
        }
    }
}
