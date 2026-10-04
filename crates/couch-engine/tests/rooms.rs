//! Rooms end to end: several engines in one process, each with its own
//! profile folder, talking over real iroh endpoints on loopback. Timings are
//! shortened; the rules are the real ones.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use couch_engine::{
    Command, Engine, EngineError, EngineOptions, Event, InvitePreview, InviteStatus, MemberLink,
    Profile, Response, RoomGone, RoomView, secrets::Unprotected,
};
use couch_net::endpoint::RelayChoice;
use couch_proto::invite::Invite;
use couch_room::{limits::LimitConfig, room::Timings};
use tempfile::TempDir;

const PASSWORD: &str = "pizza night";
const WAIT: Duration = Duration::from_secs(20);

fn options(port: u16) -> EngineOptions {
    EngineOptions {
        port: Some(port),
        relays: RelayChoice::Disabled,
        secret_store: Arc::new(Unprotected),
        limits: LimitConfig {
            first_backoff: Duration::from_millis(800),
            room_failures: 3,
            room_pause: Duration::from_secs(60),
            ..LimitConfig::default()
        },
        timings: Timings {
            repair_every: Duration::from_secs(2),
            first_redial: Duration::from_millis(200),
            max_redial: Duration::from_secs(2),
            after_shutdown: Duration::from_secs(1),
        },
        tick: Duration::from_millis(50),
        invite_valid_for: Duration::from_secs(3600),
        profile_wait: Duration::from_secs(2),
    }
}

async fn start_with(dir: &Path, name: &str, options: EngineOptions) -> Engine {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let engine = Engine::start(Profile::new("test", dir).unwrap(), options)
        .await
        .expect("engine starts");
    engine
        .handle(Command::SetDisplayName { name: name.into() })
        .await
        .unwrap();
    engine
}

async fn start(dir: &Path, name: &str, port: u16) -> Engine {
    start_with(dir, name, options(port)).await
}

/// A member with its own folder, so it can be stopped and started again.
struct Member {
    dir: TempDir,
    name: &'static str,
    /// Kept across restarts, so the addresses others saved stay valid (there
    /// is no relay to find us by id).
    port: u16,
    engine: Option<Engine>,
}

impl Member {
    async fn new(name: &'static str) -> Self {
        let port = free_port();
        Self::with_options(name, options(port)).await
    }

    async fn with_options(name: &'static str, options: EngineOptions) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let port = options.port.expect("a fixed port");
        let engine = start_with(dir.path(), name, options).await;
        Self {
            dir,
            name,
            port,
            engine: Some(engine),
        }
    }

    fn engine(&self) -> &Engine {
        self.engine.as_ref().expect("running")
    }

    fn id(&self) -> String {
        self.engine().member_id().to_string()
    }

    async fn stop(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown().await;
        }
    }

    async fn restart(&mut self) {
        self.stop().await;
        self.engine = Some(start(self.dir.path(), self.name, self.port).await);
    }

    async fn handle(&self, command: Command) -> Result<Response, EngineError> {
        self.engine().handle(command).await
    }

    async fn create(&self, name: &str) -> RoomView {
        match self
            .handle(Command::CreateRoom {
                name: name.into(),
                password: PASSWORD.into(),
            })
            .await
            .unwrap()
        {
            Response::Room(room) => room,
            other => panic!("expected a room, got {other:?}"),
        }
    }

    async fn try_invite(&self, room: &str) -> Result<String, EngineError> {
        match self
            .handle(Command::GetInvite { room: room.into() })
            .await?
        {
            Response::Invite { link } => Ok(link),
            other => panic!("expected an invite, got {other:?}"),
        }
    }

    async fn invite(&self, room: &str) -> String {
        self.try_invite(room).await.unwrap()
    }

    async fn join(&self, invite: &str, password: &str) -> Result<RoomView, EngineError> {
        match self
            .handle(Command::JoinRoom {
                invite: invite.into(),
                password: password.into(),
            })
            .await?
        {
            Response::Room(room) => Ok(room),
            other => panic!("expected a room, got {other:?}"),
        }
    }

    async fn rooms(&self) -> Vec<RoomView> {
        match self.handle(Command::ListRooms).await.unwrap() {
            Response::Rooms { rooms } => rooms,
            other => panic!("expected rooms, got {other:?}"),
        }
    }

    async fn room(&self, id: &str) -> Option<RoomView> {
        self.rooms().await.into_iter().find(|r| r.id == id)
    }

    /// Waits until `check` holds for this member's view of the room.
    async fn wait_for(&self, room: &str, what: &str, check: impl Fn(Option<&RoomView>) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = self.room(room).await;
            if check(view.as_ref()) {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "{}: timed out waiting for {what}; last view: {view:#?}",
                    self.name
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits until this member is connected to all of `others`, and, with
    /// `total`, sees exactly that many members (itself included).
    async fn wait_connected(&self, room: &str, others: &[&Member], total: Option<usize>) {
        let ids: Vec<String> = others.iter().map(|m| m.id()).collect();
        self.wait_for(room, "a connection to everyone", |view| {
            view.is_some_and(|v| {
                let connected: Vec<&String> = v
                    .members
                    .iter()
                    .filter(|m| matches!(m.link, MemberLink::Direct | MemberLink::Relay))
                    .map(|m| &m.id)
                    .collect();
                total.is_none_or(|n| v.members.len() == n)
                    && ids.iter().all(|id| connected.contains(&id))
            })
        })
        .await;
    }

    /// Whether this member still keeps the room on disk (hidden or not).
    fn keeps(&self, room: &str) -> bool {
        self.dir
            .path()
            .join("profiles/test/rooms")
            .join(format!("{room}.room"))
            .exists()
    }

    /// Waits until this member no longer keeps the room on disk.
    async fn wait_forgotten(&self, room: &str, what: &str) {
        let deadline = Instant::now() + WAIT;
        while self.keeps(room) {
            assert!(Instant::now() < deadline, "{}: {what}", self.name);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits until this member no longer has any room.
    async fn wait_roomless(&self, what: &str) {
        let deadline = Instant::now() + WAIT;
        while !self.rooms().await.is_empty() {
            assert!(Instant::now() < deadline, "{}: {what}", self.name);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn full_mesh(room: &str, members: &[&Member]) {
    for (i, member) in members.iter().enumerate() {
        let others: Vec<&Member> = members
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, m)| *m)
            .collect();
        member
            .wait_connected(room, &others, Some(members.len()))
            .await;
    }
}

/// A, the owner, with B and C, both let in by A.
async fn three() -> (Member, Member, Member, String) {
    let a = Member::new("Ana").await;
    let b = Member::new("Bia").await;
    let c = Member::new("Caio").await;
    let room = a.create("Sofá").await.id;
    let invite = a.invite(&room).await;
    b.join(&invite, PASSWORD).await.unwrap();
    c.join(&invite, PASSWORD).await.unwrap();
    full_mesh(&room, &[&a, &b, &c]).await;
    (a, b, c, room)
}

async fn stop_all(members: &mut [&mut Member]) {
    for m in members.iter_mut() {
        m.stop().await;
    }
}

fn refused_because(err: &EngineError, words: &str) -> bool {
    matches!(err, EngineError::Refused(why) if why.contains(words))
}

#[tokio::test]
async fn three_members_join_and_see_each_other() {
    let (mut a, mut b, mut c, room) = three().await;
    let view = c.room(&room).await.unwrap();
    assert_eq!(view.name, "Sofá");
    assert!(!view.is_owner);
    let names: Vec<&str> = view.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Caio", "Ana", "Bia"], "me first, then by name");
    assert!(view.members.iter().any(|m| m.name == "Ana" && m.is_owner));
    assert!(a.room(&room).await.unwrap().is_owner);

    // A renamed member is seen by everyone.
    b.handle(Command::SetDisplayName {
        name: "Beatriz".into(),
    })
    .await
    .unwrap();
    for m in [&a, &c] {
        m.wait_for(&room, "the new name", |v| {
            v.is_some_and(|v| v.members.iter().any(|m| m.name == "Beatriz"))
        })
        .await;
    }
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test]
async fn only_the_owner_makes_invites() {
    let (mut a, mut b, mut c, room) = three().await;
    let err = b.try_invite(&room).await.unwrap_err();
    assert!(matches!(err, EngineError::Room(_)), "{err}");
    let err = b
        .handle(Command::RevokeInvites { room: room.clone() })
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Room(_)), "{err}");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test]
async fn a_wrong_password_is_refused_then_rate_limited() {
    let mut a = Member::new("Ana").await;
    let mut b = Member::new("Bia").await;
    let room = a.create("Couch").await.id;
    let invite = a.invite(&room).await;

    let wrong = b.join(&invite, "pizza nite").await.unwrap_err();
    assert!(matches!(wrong, EngineError::WrongPassword), "{wrong}");
    // Straight away, even the right password is turned away: B's id is in
    // backoff.
    let limited = b.join(&invite, PASSWORD).await.unwrap_err();
    assert!(refused_because(&limited, "too many"), "{limited}");
    tokio::time::sleep(Duration::from_millis(900)).await;
    b.join(&invite, PASSWORD).await.unwrap();
    full_mesh(&room, &[&a, &b]).await;
    stop_all(&mut [&mut a, &mut b]).await;
}

#[tokio::test]
async fn many_wrong_passwords_pause_joining() {
    let mut a = Member::new("Ana").await;
    let mut b = Member::new("Bia").await;
    let room = a.create("Couch").await.id;
    let invite = a.invite(&room).await;
    b.join(&invite, PASSWORD).await.unwrap();
    full_mesh(&room, &[&a, &b]).await;
    let mut events = a.engine().subscribe();

    // Three strangers, each with a fresh identity, guess wrong.
    for i in 0..3 {
        let mut guesser = Member::new("Guess").await;
        let err = guesser
            .join(&invite, &format!("guess {i}"))
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::WrongPassword), "{err}");
        guesser.stop().await;
    }
    let paused = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(Event::JoinsPaused { room: r, minutes }) = events.recv().await {
                return (r, minutes);
            }
        }
    })
    .await
    .expect("the owner is warned");
    assert_eq!(paused, (room.clone(), 1));
    assert!(a.room(&room).await.unwrap().joins_paused_for_secs.is_some());

    // A newcomer with the right password is turned away for now...
    let mut late = Member::new("Late").await;
    let err = late.join(&invite, PASSWORD).await.unwrap_err();
    assert!(refused_because(&err, "too many"), "{err}");
    // ...but a member coming back gets in: members don't need the owner's
    // password check at all.
    b.restart().await;
    full_mesh(&room, &[&a, &b]).await;
    stop_all(&mut [&mut a, &mut b, &mut late]).await;
}

#[tokio::test]
async fn an_invite_dies_after_five_wrong_passwords() {
    let generous = |port| EngineOptions {
        limits: LimitConfig {
            room_failures: 100,
            ..options(port).limits
        },
        ..options(port)
    };
    let mut a = Member::with_options("Ana", generous(free_port())).await;
    let room = a.create("Couch").await.id;
    let invite = a.invite(&room).await;
    for i in 0..5 {
        let mut guesser = Member::new("Guess").await;
        let err = guesser
            .join(&invite, &format!("guess {i}"))
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::WrongPassword), "{err}");
        guesser.stop().await;
    }
    let mut b = Member::new("Bia").await;
    let err = b.join(&invite, PASSWORD).await.unwrap_err();
    assert!(refused_because(&err, "expired or was revoked"), "{err}");
    // Another invite works.
    b.join(&a.invite(&room).await, PASSWORD).await.unwrap();
    stop_all(&mut [&mut a, &mut b]).await;
}

#[tokio::test]
async fn a_banned_member_is_removed_and_kept_out() {
    let (mut a, mut b, mut c, room) = three().await;
    let old_invite = a.invite(&room).await;
    let mut c_events = c.engine().subscribe();

    let not_owner = b
        .handle(Command::BanMember {
            room: room.clone(),
            member: c.id(),
        })
        .await
        .unwrap_err();
    assert!(matches!(not_owner, EngineError::Room(_)), "{not_owner}");

    a.handle(Command::BanMember {
        room: room.clone(),
        member: c.id(),
    })
    .await
    .unwrap();
    let removed = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(Event::RoomRemoved { room: r, reason }) = c_events.recv().await {
                return (r, reason);
            }
        }
    })
    .await
    .expect("C hears it from the owner's policy");
    assert_eq!(removed, (room.clone(), RoomGone::Banned));
    assert!(c.rooms().await.is_empty());
    full_mesh(&room, &[&a, &b]).await;

    // C can't come back, even with a brand new invite...
    let fresh = a.invite(&room).await;
    let err = c.join(&fresh, PASSWORD).await.unwrap_err();
    assert!(refused_because(&err, "banned"), "{err}");
    // ...and the ban revoked the invites made before it, which C may hold.
    let mut d = Member::new("Davi").await;
    let err = d.join(&old_invite, PASSWORD).await.unwrap_err();
    assert!(refused_because(&err, "expired or was revoked"), "{err}");
    d.join(&fresh, PASSWORD).await.unwrap();
    full_mesh(&room, &[&a, &b, &d]).await;
    stop_all(&mut [&mut a, &mut b, &mut c, &mut d]).await;
}

/// A member banned while it was offline is shown the owner's signed policy
/// when it comes back, and forgets the room.
#[tokio::test]
async fn a_member_banned_while_offline_finds_out_on_return() {
    let (mut a, mut b, mut c, room) = three().await;
    let c_id = c.id();
    c.stop().await;
    a.handle(Command::BanMember {
        room: room.clone(),
        member: c_id,
    })
    .await
    .unwrap();
    c.restart().await;
    c.wait_roomless("C never learned of the ban").await;
    full_mesh(&room, &[&a, &b]).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test]
async fn joining_needs_the_owner_online() {
    let mut a = Member::new("Ana").await;
    let mut b = Member::new("Bia").await;
    let room = a.create("Couch").await.id;
    let invite = a.invite(&room).await;
    a.stop().await;
    let err = b.join(&invite, PASSWORD).await.unwrap_err();
    assert!(matches!(err, EngineError::OwnerOffline), "{err}");
    assert!(b.rooms().await.is_empty());
    stop_all(&mut [&mut a, &mut b]).await;
}

#[tokio::test]
async fn members_reconnect_without_the_owner() {
    let (mut a, mut b, mut c, room) = three().await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
    b.restart().await;
    c.restart().await;
    // The owner is a member they can't reach, but they reach each other.
    b.wait_connected(&room, &[&c], Some(3)).await;
    c.wait_connected(&room, &[&b], Some(3)).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// A member who was away when someone joined learns of the newcomer from
/// the newcomer itself, which shows the owner's newer member list.
#[tokio::test]
async fn a_member_who_missed_a_join_meets_the_newcomer() {
    let (mut a, mut b, mut c, room) = three().await;
    c.stop().await;
    let mut d = Member::new("Davi").await;
    d.join(&a.invite(&room).await, PASSWORD).await.unwrap();
    // C is offline but still a member: four in all.
    a.wait_connected(&room, &[&b, &d], Some(4)).await;
    b.wait_connected(&room, &[&a, &d], Some(4)).await;
    d.wait_connected(&room, &[&a, &b], Some(4)).await;
    a.stop().await;
    b.stop().await;
    c.restart().await;
    c.wait_connected(&room, &[&d], Some(4)).await;
    d.wait_connected(&room, &[&c], Some(4)).await;
    stop_all(&mut [&mut a, &mut b, &mut c, &mut d]).await;
}

/// Everyone restarting at once dials everyone at once: each pair ends up
/// with one connection, and both ends agree which. For a moment each side
/// may hold its own dial; once both dials are accepted on both sides, they
/// settle on the same one (the lower id's, a rule the room tests check
/// deterministically).
#[tokio::test]
async fn simultaneous_restarts_settle_on_one_connection_per_pair() {
    let (mut a, mut b, mut c, room) = three().await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
    let (ea, eb, ec) = tokio::join!(
        start(a.dir.path(), "Ana", a.port),
        start(b.dir.path(), "Bia", b.port),
        start(c.dir.path(), "Caio", c.port)
    );
    a.engine = Some(ea);
    b.engine = Some(eb);
    c.engine = Some(ec);
    full_mesh(&room, &[&a, &b, &c]).await;

    let deadline = Instant::now() + WAIT;
    loop {
        let mut seen = std::collections::BTreeMap::new();
        for m in [&a, &b, &c] {
            for (peer, dialed_by_me) in m.engine().connections(&room).await {
                seen.insert((m.id(), peer), dialed_by_me);
            }
        }
        let agreed = seen.len() == 6
            && seen.iter().all(|((me, peer), dialed_by_me)| {
                seen.get(&(peer.clone(), me.clone())) == Some(&!dialed_by_me)
            });
        if agreed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the pairs never agreed on their connections: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test]
async fn leaving_is_final_and_remembered_rooms_survive_restarts() {
    let (mut a, mut b, mut c, room) = three().await;
    c.handle(Command::LeaveRoom { room: room.clone() })
        .await
        .unwrap();
    assert!(c.rooms().await.is_empty());
    a.wait_connected(&room, &[&b], Some(2)).await;
    b.wait_connected(&room, &[&a], Some(2)).await;
    // After restarts, A and B still have the room; C doesn't.
    a.restart().await;
    b.restart().await;
    c.restart().await;
    full_mesh(&room, &[&a, &b]).await;
    assert!(c.rooms().await.is_empty());
    assert!(!c.keeps(&room), "the others heard at once");

    // C is no longer a member, but the owner can still keep it out.
    a.handle(Command::BanMember {
        room: room.clone(),
        member: c.id(),
    })
    .await
    .unwrap();
    let err = c.join(&a.invite(&room).await, PASSWORD).await.unwrap_err();
    assert!(refused_because(&err, "banned"), "{err}");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// C leaves while nobody else is online. The room disappears from C's list
/// at once, but C keeps it, hidden, until someone has heard.
#[tokio::test]
async fn a_member_who_leaves_alone_tells_the_room_later() {
    let (mut a, mut b, mut c, room) = three().await;
    stop_all(&mut [&mut a, &mut b]).await;
    c.handle(Command::LeaveRoom { room: room.clone() })
        .await
        .unwrap();
    assert!(c.rooms().await.is_empty());
    assert!(c.keeps(&room), "nobody has heard yet");
    // It keeps trying across a restart.
    c.restart().await;
    assert!(c.rooms().await.is_empty());
    a.restart().await;
    b.restart().await;
    c.wait_forgotten(&room, "C never got the word out").await;
    full_mesh(&room, &[&a, &b]).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// The owner closes the room while C is offline: C hears it when it comes
/// back, and only then does the owner forget the room.
#[tokio::test]
async fn a_closed_room_reaches_a_member_who_was_offline() {
    let (mut a, mut b, mut c, room) = three().await;
    c.stop().await;
    a.handle(Command::LeaveRoom { room: room.clone() })
        .await
        .unwrap();
    assert!(a.rooms().await.is_empty());
    b.wait_roomless("B kept a closed room").await;
    assert!(a.keeps(&room), "C hasn't heard yet");
    a.restart().await;
    c.restart().await;
    c.wait_roomless("C kept a closed room").await;
    a.wait_forgotten(&room, "the owner never told C").await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test]
async fn the_owner_leaving_closes_the_room() {
    let (mut a, mut b, mut c, room) = three().await;
    let mut b_events = b.engine().subscribe();
    a.handle(Command::LeaveRoom { room: room.clone() })
        .await
        .unwrap();
    let removed = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(Event::RoomRemoved { reason, .. }) = b_events.recv().await {
                return reason;
            }
        }
    })
    .await
    .expect("B is told");
    assert_eq!(removed, RoomGone::Removed);
    c.wait_roomless("C kept a closed room").await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test]
async fn revoked_and_expired_invites_stop_working() {
    let short = |port| EngineOptions {
        invite_valid_for: Duration::from_secs(1),
        ..options(port)
    };
    let mut a = Member::with_options("Ana", short(free_port())).await;
    let mut b = Member::new("Bia").await;
    let room = a.create("Couch").await.id;

    let invite = a.invite(&room).await;
    a.handle(Command::RevokeInvites { room: room.clone() })
        .await
        .unwrap();
    let err = b.join(&invite, PASSWORD).await.unwrap_err();
    assert!(refused_because(&err, "expired or was revoked"), "{err}");

    let invite = a.invite(&room).await;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let err = b.join(&invite, PASSWORD).await.unwrap_err();
    assert!(refused_because(&err, "expired or was revoked"), "{err}");

    // Invites survive the owner restarting (within their lifetime, here the
    // usual hour).
    a.restart().await;
    let invite = a.invite(&room).await;
    a.restart().await;
    b.join(&invite, PASSWORD).await.unwrap();
    stop_all(&mut [&mut a, &mut b]).await;
}

/// With relays configured, an invite never carries IP addresses, even right
/// after startup before the relay is known. (The relay here doesn't exist,
/// so the invite either names it or isn't made at all.)
#[tokio::test]
async fn invites_carry_no_ip_addresses_when_relays_are_configured() {
    let dir = tempfile::tempdir().unwrap();
    let options = EngineOptions {
        relays: RelayChoice::Custom(vec!["https://relay.invalid./".parse().unwrap()]),
        ..options(free_port())
    };
    let engine = Engine::start(Profile::new("test", dir.path()).unwrap(), options)
        .await
        .unwrap();
    let Response::Room(room) = engine
        .handle(Command::CreateRoom {
            name: "Couch".into(),
            password: PASSWORD.into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected a room");
    };
    match engine.handle(Command::GetInvite { room: room.id }).await {
        Ok(Response::Invite { link }) => {
            let invite = Invite::parse(&link).unwrap();
            assert!(invite.addr.ips.is_empty(), "{invite:?}");
        }
        Err(EngineError::NotOnline) => {}
        other => panic!("unexpected: {other:?}"),
    }
    engine.shutdown().await;
}

async fn preview(m: &Member, invite: String) -> Result<InvitePreview, EngineError> {
    match m.handle(Command::PreviewInvite { invite }).await? {
        Response::InvitePreview(preview) => Ok(preview),
        other => panic!("expected a preview, got {other:?}"),
    }
}

/// The join screen: a link the app was opened with waits for the UI, and an
/// invite is previewed before any password is asked for.
#[tokio::test]
async fn an_opened_invite_is_previewed_before_joining() {
    let mut a = Member::new("Ana").await;
    let mut b = Member::new("Bia").await;
    let room = a.create("Sofá").await.id;
    let invite = a.invite(&room).await;

    // Opening a link only keeps it for the UI, once.
    let mut events = b.engine().subscribe();
    b.engine().open_link(&invite);
    assert_eq!(events.recv().await.unwrap(), Event::InviteOpened);
    let take = || b.handle(Command::TakeOpenedInvite);
    assert_eq!(
        take().await.unwrap(),
        Response::OpenedInvite {
            link: Some(invite.clone())
        }
    );
    assert_eq!(take().await.unwrap(), Response::OpenedInvite { link: None });
    b.engine()
        .open_link(&"x".repeat(couch_proto::invite::MAX_CHARS + 1));
    assert_eq!(take().await.unwrap(), Response::OpenedInvite { link: None });

    let seen = preview(&b, invite.clone()).await.unwrap();
    assert_eq!(
        (seen.room.as_str(), seen.name.as_str(), seen.status),
        (room.as_str(), "Sofá", InviteStatus::Ready)
    );
    assert!((3590..=3600).contains(&seen.expires_in_secs), "{seen:?}");
    assert_eq!(
        preview(&a, invite.clone()).await.unwrap().status,
        InviteStatus::Own
    );
    let err = preview(&b, "hello there".into()).await.unwrap_err();
    assert!(matches!(err, EngineError::Invite(_)), "{err}");

    b.join(&invite, PASSWORD).await.unwrap();
    assert_eq!(
        preview(&b, invite.clone()).await.unwrap().status,
        InviteStatus::Member
    );
    // Expiry has its own owner: an invite that lasts a second can't also be
    // joined with reliably (expiry counts whole seconds).
    let short = EngineOptions {
        invite_valid_for: Duration::from_secs(1),
        ..options(free_port())
    };
    let mut c = Member::with_options("Caio", short).await;
    let short_room = c.create("Rápido").await.id;
    let short_invite = c.invite(&short_room).await;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let seen = preview(&b, short_invite).await.unwrap();
    assert_eq!(
        (seen.status, seen.expires_in_secs),
        (InviteStatus::Expired, 0)
    );
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

async fn change(m: &Member, room: &str, password: &str) -> Result<Response, EngineError> {
    m.handle(Command::ChangePassword {
        room: room.into(),
        password: password.into(),
    })
    .await
}

/// Only the owner changes the password; it lasts across restarts, and
/// members stay.
#[tokio::test]
async fn the_owner_changes_the_password() {
    let mut a = Member::new("Ana").await;
    let mut b = Member::new("Bia").await;
    let mut c = Member::new("Caio").await;
    let room = a.create("Sofá").await.id;
    let invite = a.invite(&room).await;
    b.join(&invite, PASSWORD).await.unwrap();

    let err = change(&b, &room, "mine now").await.unwrap_err();
    assert!(matches!(err, EngineError::Room(_)), "{err}");
    let err = change(&a, &room, "  ").await.unwrap_err();
    assert!(matches!(err, EngineError::EmptyPassword), "{err}");
    change(&a, &room, "board games").await.unwrap();
    a.restart().await;

    let err = c.join(&invite, PASSWORD).await.unwrap_err();
    assert!(matches!(err, EngineError::WrongPassword), "{err}");
    let mut d = Member::new("Duda").await;
    d.join(&invite, "board games").await.unwrap();
    full_mesh(&room, &[&a, &b, &d]).await;
    stop_all(&mut [&mut a, &mut b, &mut c, &mut d]).await;
}

#[tokio::test]
async fn bad_input_is_refused_politely() {
    let mut a = Member::new("Ana").await;
    let err = a.join("hello there", PASSWORD).await.unwrap_err();
    assert!(matches!(err, EngineError::Invite(_)), "{err}");
    let err = a
        .handle(Command::CreateRoom {
            name: "Couch".into(),
            password: "   ".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::EmptyPassword), "{err}");
    let err = a
        .handle(Command::CreateRoom {
            name: "".into(),
            password: PASSWORD.into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Room(_)), "{err}");
    let err = a.try_invite(&"00".repeat(16)).await.unwrap_err();
    assert!(matches!(err, EngineError::UnknownRoom), "{err}");
    a.stop().await;
}

/// Joining with an invite that names only the relay, as real invites do once
/// a relay is configured. Needs the internet: iroh's public relays.
#[tokio::test]
#[ignore = "needs internet access (iroh's public relays)"]
async fn joins_through_the_public_relay() {
    let online = |port| EngineOptions {
        relays: RelayChoice::N0Public,
        ..options(port)
    };
    let mut a = Member::with_options("Ana", online(free_port())).await;
    let mut b = Member::with_options("Bia", online(free_port())).await;
    let room = a.create("Couch").await.id;
    // Waits for A's relay if needed, and never lists an IP address.
    let invite = a.invite(&room).await;
    let parsed = Invite::parse(&invite).unwrap();
    assert!(parsed.addr.relay.is_some());
    assert!(parsed.addr.ips.is_empty(), "no IP addresses in invites");
    let joined = b.join(&invite, PASSWORD).await.unwrap();
    assert_eq!(joined.members.len(), 2);
    let link = joined
        .members
        .iter()
        .find(|m| m.name == "Ana")
        .unwrap()
        .link;
    eprintln!("B reaches A: {link:?}");
    stop_all(&mut [&mut a, &mut b]).await;
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
