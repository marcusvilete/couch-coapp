//! The update notice (M26c-2, ADR 0014): a build that names its releases
//! (`build_config::release_source`) looks at GitHub's latest release at most
//! once a day, a few minutes after start, never while hosting or watching,
//! and says when a newer version is out until the user dismisses it for
//! that version. Nothing is downloaded or installed. The pages the app
//! opens for it (the release, the latest release, a problem's report) are
//! made here from the build's repository, never from the webview, and of
//! GitHub's reply only the version its tag names once it parsed as numbers
//! and dots.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{
    EngineError, Event, NewVersionView, UpdatesView, WebPage, build_config::ReleaseSource,
    remote::unix_now, rooms, store,
};

/// The first look comes this long after start: never in the way of a start.
const FIRST_LOOK: Duration = Duration::from_secs(3 * 60);
/// With `COUCH_RELEASES_URL` standing in (development and test builds),
/// soon after start, and once every start.
const STAND_IN_FIRST_LOOK: Duration = Duration::from_secs(10);
/// At most once a day.
const EVERY_SECS: u64 = 24 * 60 * 60;
/// A look that failed is tried again this much later.
const RETRY_SECS: u64 = 60 * 60;
/// How often the task wakes to see whether a look is due (and the app no
/// longer busy).
const WAKE: Duration = Duration::from_secs(10 * 60);
/// GitHub's reply carries the notes and the assets: tens of kilobytes.
const MAX_REPLY: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

/// What `updates.json` keeps between starts. Never backed up: this PC's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UpdateState {
    /// When GitHub last answered, in Unix seconds.
    pub checked_at: u64,
    /// The newest release it named then.
    pub latest: Option<Release>,
    /// The version whose notice the user dismissed.
    pub dismissed: Option<String>,
}

/// A release, as its tag and notes say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Release {
    /// Such as "0.40.0": the tag without its `v`.
    pub version: String,
    /// The wire revision its notes' first line names, if it does.
    pub wire: Option<u16>,
}

/// The part of GitHub's latest-release reply we read.
#[derive(Deserialize)]
struct Reply {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
}

/// Reads GitHub's reply. Its "latest" is never a draft or a pre-release;
/// one that says it is, or whose tag isn't `v` and a version, is refused.
pub(crate) fn parse_release(bytes: &[u8]) -> Result<Release, String> {
    let reply: Reply = serde_json::from_slice(bytes).map_err(|e| format!("unreadable: {e}"))?;
    if reply.draft || reply.prerelease {
        return Err("a draft or a pre-release".into());
    }
    let version = reply
        .tag_name
        .strip_prefix('v')
        .filter(|v| parse_version(v).is_some())
        .ok_or_else(|| "its tag doesn't name a version".to_string())?;
    Ok(Release {
        version: version.to_string(),
        wire: reply.body.as_deref().and_then(wire_of),
    })
}

/// `major.minor.patch`, each a number: all `cargo xtask bump` makes.
fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.split('.');
    let mut version = [0; 3];
    for part in &mut version {
        let digits = parts.next()?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *part = digits.parse().ok()?;
    }
    parts.next().is_none().then_some(version)
}

/// `theirs` is a newer version than `ours`.
pub(crate) fn newer(theirs: &str, ours: &str) -> bool {
    match (parse_version(theirs), parse_version(ours)) {
        (Some(theirs), Some(ours)) => theirs > ours,
        _ => false,
    }
}

/// The notes' first line, `<!-- couch-wire-revision: N -->`.
fn wire_of(body: &str) -> Option<u16> {
    let line = body.lines().next()?.trim();
    let inner = line.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let n = inner
        .strip_prefix(couch_proto::RELEASE_WIRE_MARKER)?
        .strip_prefix(':')?
        .trim();
    n.parse().ok()
}

/// Whether a look is due at `now`: never looked, a day after the last
/// answer, or the clock went back past it.
fn due(state: &UpdateState, now: u64) -> bool {
    state.checked_at == 0
        || now >= state.checked_at.saturating_add(EVERY_SECS)
        || now < state.checked_at
}

/// The engine's side of the notice: where it looks, and what it found.
pub(crate) struct Updates {
    source: Option<ReleaseSource>,
    dir: PathBuf,
    state: Mutex<UpdateState>,
    /// `updates.json` is read and written. Never with the stand-in: a
    /// development build shares its profile with the installed app, which
    /// would then show the stand-in's version and not look for a day.
    persist: bool,
    /// Counts the views made, so the UI keeps the newer of two (a command's
    /// reply and an event reach it in either order).
    revision: AtomicU64,
}

impl Updates {
    pub(crate) fn new(source: Option<ReleaseSource>, dir: PathBuf) -> Self {
        let persist = !source.as_ref().is_some_and(|s| s.stand_in);
        let state = if persist {
            store::updates(&dir)
        } else {
            UpdateState::default()
        };
        Self {
            source,
            dir,
            state: Mutex::new(state),
            persist,
            revision: AtomicU64::new(0),
        }
    }

    fn save(&self, state: &UpdateState) -> Result<(), EngineError> {
        if self.persist {
            store::save_updates(&self.dir, state)?;
        }
        Ok(())
    }

    /// Whether this build names its releases (or a stand-in does).
    pub(crate) fn has_releases(&self) -> bool {
        self.source.is_some()
    }

    /// What Settings and the shell's strip show: the notice only while the
    /// switch is on.
    pub(crate) fn view(&self) -> UpdatesView {
        let on = store::app(&self.dir).updates;
        let state = self.state.lock().expect("update state");
        UpdatesView {
            revision: self.revision.fetch_add(1, Ordering::SeqCst) + 1,
            checks: self.has_releases(),
            on,
            new_version: if on && self.has_releases() {
                notice(&state, env!("CARGO_PKG_VERSION"))
            } else {
                None
            },
        }
    }

    /// The user dismissed the notice for `version`, the one it shows: it
    /// stays away until a newer one.
    pub(crate) fn dismiss(&self, version: &str) -> Result<(), EngineError> {
        let mut state = self.state.lock().expect("update state");
        if !notice(&state, env!("CARGO_PKG_VERSION")).is_some_and(|n| n.version == version) {
            return Err(EngineError::Refused("the notice doesn't show that version"));
        }
        state.dismissed = Some(version.to_string());
        self.save(&state)
    }

    /// The address of `page`, made from the build's repository, and for the
    /// new version's page the version GitHub's tag named once it parsed as
    /// numbers and dots.
    pub(crate) fn page(&self, page: WebPage) -> Result<String, EngineError> {
        let source = self
            .source
            .as_ref()
            .ok_or(EngineError::Refused("this build names no releases"))?;
        let repository = &source.repository;
        Ok(match page {
            WebPage::NewVersion => {
                let state = self.state.lock().expect("update state");
                let release = state
                    .latest
                    .as_ref()
                    .filter(|r| newer(&r.version, env!("CARGO_PKG_VERSION")))
                    .ok_or(EngineError::Refused("no newer version is known"))?;
                format!("{repository}/releases/tag/v{}", release.version)
            }
            WebPage::Releases => format!("{repository}/releases/latest"),
            WebPage::ReportProblem => format!("{repository}/issues/new?template=problem.yml"),
        })
    }

    /// Takes GitHub's answer: `true` if what the notice shows changed.
    fn heard(&self, release: Release, now: u64) -> bool {
        let mut state = self.state.lock().expect("update state");
        let before = notice(&state, env!("CARGO_PKG_VERSION"));
        state.checked_at = now;
        state.latest = Some(release);
        if let Err(e) = self.save(&state) {
            tracing::warn!("couldn't save what the update notice found: {e}");
        }
        notice(&state, env!("CARGO_PKG_VERSION")) != before
    }

    /// GitHub answered with a release we don't take (its tag isn't a
    /// version, say): looked at all the same, so not again for a day.
    fn refused(&self, now: u64) {
        let mut state = self.state.lock().expect("update state");
        state.checked_at = now;
        if let Err(e) = self.save(&state) {
            tracing::warn!("couldn't save when the update notice looked: {e}");
        }
    }

    fn due(&self, now: u64) -> bool {
        due(&self.state.lock().expect("update state"), now)
    }
}

/// The notice, if the latest release we know is newer than `ours` and than
/// the version dismissed.
fn notice(state: &UpdateState, ours: &str) -> Option<NewVersionView> {
    let latest = state.latest.as_ref()?;
    if !newer(&latest.version, ours)
        || state
            .dismissed
            .as_deref()
            .is_some_and(|dismissed| !newer(&latest.version, dismissed))
    {
        return None;
    }
    Some(NewVersionView {
        version: latest.version.clone(),
        wire_changed: latest
            .wire
            .is_some_and(|wire| wire != couch_proto::PROTOCOL_MINOR),
    })
}

/// Looks for new versions while the engine runs (a task of its own). The
/// switch is read before every look, so turning it off stops the next one.
pub(crate) async fn run(
    updates: Arc<Updates>,
    events: broadcast::Sender<Event>,
    actor: mpsc::Sender<rooms::Input>,
) {
    let Some(source) = updates.source.clone() else {
        return;
    };
    let pace = if source.stand_in {
        Pace {
            first: STAND_IN_FIRST_LOOK,
            wake: STAND_IN_FIRST_LOOK,
            retry_secs: 60,
        }
    } else {
        Pace {
            first: FIRST_LOOK,
            wake: WAKE,
            retry_secs: RETRY_SECS,
        }
    };
    run_paced(updates, source, events, actor, pace).await;
}

/// When the task looks: after start, how often it wakes, and how long a
/// failure waits.
struct Pace {
    first: Duration,
    wake: Duration,
    retry_secs: u64,
}

async fn run_paced(
    updates: Arc<Updates>,
    source: ReleaseSource,
    events: broadcast::Sender<Event>,
    actor: mpsc::Sender<rooms::Input>,
    pace: Pace,
) {
    tokio::time::sleep(pace.first).await;
    let mut retry_at = 0;
    loop {
        let now = unix_now();
        // The stand-in looks once every start; GitHub once a day.
        let wanted = source.stand_in || updates.due(now);
        if wanted && now >= retry_at && store::app(&updates.dir).updates {
            let (tx, rx) = oneshot::channel();
            if actor.send(rooms::Input::Busy(tx)).await.is_err() {
                return;
            }
            let Ok(busy) = rx.await else { return };
            if !busy {
                match look(&source).await {
                    Ok(release) => {
                        tracing::info!(
                            "the latest release is {} (wire revision {:?})",
                            release.version,
                            release.wire
                        );
                        if updates.heard(release, unix_now()) {
                            let _ = events.send(Event::Updates(updates.view()));
                        }
                        if source.stand_in {
                            return;
                        }
                    }
                    Err(Failed::Refused(e)) => {
                        tracing::info!("GitHub's latest release isn't one to tell of: {e}");
                        updates.refused(unix_now());
                        if source.stand_in {
                            return;
                        }
                    }
                    Err(Failed::Unreachable(e)) => {
                        tracing::info!("couldn't look for a new version: {e}");
                        retry_at = unix_now() + pace.retry_secs;
                    }
                }
            }
        }
        tokio::time::sleep(pace.wake).await;
    }
}

/// Why a look found nothing.
#[derive(Debug)]
enum Failed {
    /// No answer: tried again an hour later.
    Unreachable(String),
    /// An answer we don't take: not again for a day.
    Refused(String),
}

async fn look(source: &ReleaseSource) -> Result<Release, Failed> {
    let bytes = couch_net::web::get(
        &source.api,
        "application/vnd.github+json",
        MAX_REPLY,
        TIMEOUT,
        source.stand_in,
    )
    .await
    .map_err(|e| Failed::Unreachable(e.to_string()))?;
    parse_release(&bytes).map_err(Failed::Refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/github-latest-release.json");

    fn release(version: &str, wire: Option<u16>) -> Release {
        Release {
            version: version.into(),
            wire,
        }
    }

    #[test]
    fn reads_githubs_reply() {
        let release = parse_release(FIXTURE.as_bytes()).unwrap();
        assert_eq!(release, self::release("99.0.0", Some(99)));
    }

    /// GitHub itself, over TLS (needs internet): a repository with releases
    /// (the drivers' we bundle), since ours may have none yet.
    #[tokio::test]
    #[ignore = "needs internet"]
    async fn reads_githubs_real_reply() {
        let source = ReleaseSource {
            api: "https://api.github.com/repos/nefarius/ViGEmBus/releases/latest".into(),
            repository: "https://github.com/nefarius/ViGEmBus".into(),
            stand_in: false,
        };
        let release = look(&source).await.unwrap();
        assert!(parse_version(&release.version).is_some(), "{release:?}");
    }

    #[test]
    fn refuses_drafts_pre_releases_and_other_tags() {
        let with = |from: &str, to: &str| FIXTURE.replacen(from, to, 1);
        for text in [
            with("\"draft\": false", "\"draft\": true"),
            with("\"prerelease\": false", "\"prerelease\": true"),
            with("\"tag_name\": \"v99.0.0\"", "\"tag_name\": \"99.0.0\""),
            with("\"tag_name\": \"v99.0.0\"", "\"tag_name\": \"v99.0.0-rc1\""),
            with("\"tag_name\": \"v99.0.0\"", "\"tag_name\": \"v99.0\""),
            "{}".to_string(),
            "not json".to_string(),
        ] {
            assert!(parse_release(text.as_bytes()).is_err(), "{text}");
        }
    }

    #[test]
    fn the_wire_revision_comes_from_the_first_line_alone() {
        assert_eq!(wire_of("<!-- couch-wire-revision: 21 -->\nText"), Some(21));
        assert_eq!(wire_of("<!--couch-wire-revision:7-->"), Some(7));
        assert_eq!(wire_of("Text\n<!-- couch-wire-revision: 21 -->"), None);
        assert_eq!(wire_of("<!-- couch-wire-revision: x -->"), None);
        assert_eq!(wire_of("<!-- something-else: 21 -->"), None);
        assert_eq!(wire_of(""), None);
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.40.0", "0.39.0"));
        assert!(newer("0.39.10", "0.39.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.39.0", "0.39.0"));
        assert!(!newer("0.38.9", "0.39.0"));
        assert!(!newer("0.40", "0.39.0"));
        assert!(!newer("0.40.0.1", "0.39.0"));
        assert!(!newer("0.40.+0", "0.39.0"));
    }

    #[test]
    fn the_notice_shows_a_newer_version_until_dismissed() {
        let ours = "0.39.0";
        let mut state = UpdateState {
            latest: Some(release("0.40.0", Some(couch_proto::PROTOCOL_MINOR))),
            ..UpdateState::default()
        };
        assert_eq!(
            notice(&state, ours),
            Some(NewVersionView {
                version: "0.40.0".into(),
                wire_changed: false
            })
        );
        state.dismissed = Some("0.40.0".into());
        assert_eq!(notice(&state, ours), None);
        // A newer one comes back.
        state.latest = Some(release("0.41.0", Some(couch_proto::PROTOCOL_MINOR + 1)));
        assert_eq!(
            notice(&state, ours),
            Some(NewVersionView {
                version: "0.41.0".into(),
                wire_changed: true
            })
        );
        // Not ours to say without the line.
        state.latest = Some(release("0.41.0", None));
        assert!(!notice(&state, ours).unwrap().wire_changed);
        // Nothing older or the same.
        state.latest = Some(release("0.39.0", Some(1)));
        assert_eq!(notice(&state, ours), None);
        // Nor older than the version dismissed (GitHub's latest went back).
        state.dismissed = Some("0.41.0".into());
        state.latest = Some(release("0.40.0", None));
        assert_eq!(notice(&state, ours), None);
    }

    #[test]
    fn a_look_is_due_once_a_day() {
        let state = UpdateState {
            checked_at: 1_000_000,
            ..UpdateState::default()
        };
        assert!(!due(&state, 1_000_000 + EVERY_SECS - 1));
        assert!(due(&state, 1_000_000 + EVERY_SECS));
        assert!(due(&state, 999_999), "the clock went back");
        assert!(due(&UpdateState::default(), 1));
    }

    fn updates(dir: &std::path::Path) -> Updates {
        Updates::new(
            Some(ReleaseSource {
                api: "https://api.github.com/repos/owner/name/releases/latest".into(),
                repository: "https://github.com/owner/name".into(),
                stand_in: false,
            }),
            dir.to_path_buf(),
        )
    }

    #[test]
    fn pages_come_from_the_repository_alone() {
        let dir = tempfile::tempdir().unwrap();
        let updates = updates(dir.path());
        assert_eq!(
            updates.page(WebPage::Releases).unwrap(),
            "https://github.com/owner/name/releases/latest"
        );
        assert_eq!(
            updates.page(WebPage::ReportProblem).unwrap(),
            "https://github.com/owner/name/issues/new?template=problem.yml"
        );
        assert!(updates.page(WebPage::NewVersion).is_err(), "none known");
        updates.heard(release("99.0.0", None), 1);
        assert_eq!(
            updates.page(WebPage::NewVersion).unwrap(),
            "https://github.com/owner/name/releases/tag/v99.0.0"
        );
        let none = Updates::new(None, dir.path().to_path_buf());
        assert!(none.page(WebPage::Releases).is_err());
        assert!(!none.view().checks);
    }

    #[test]
    fn what_it_found_and_dismissed_lasts_across_starts() {
        let dir = tempfile::tempdir().unwrap();
        let first = updates(dir.path());
        assert!(first.heard(release("99.0.0", Some(1)), 5));
        assert!(!first.heard(release("99.0.0", Some(1)), 6), "nothing new");
        let view = first.view();
        assert!(view.checks && view.on);
        assert_eq!(view.new_version.unwrap().version, "99.0.0");
        first.dismiss("99.0.0").unwrap();

        let again = updates(dir.path());
        assert_eq!(again.view().new_version, None);
        assert!(!again.due(7));
        assert!(again.dismiss("99.0.0").is_err(), "not shown");
        // A newer one shows again.
        assert!(again.heard(release("99.1.0", Some(1)), 8));
        assert_eq!(again.view().new_version.unwrap().version, "99.1.0");
        assert!(again.dismiss("99.0.0").is_err(), "only the version shown");
        // A reply we don't take counts as a look.
        again.refused(EVERY_SECS * 2);
        assert!(!again.due(EVERY_SECS * 2 + 1));
    }

    #[test]
    fn views_count_up() {
        let dir = tempfile::tempdir().unwrap();
        let updates = updates(dir.path());
        let first = updates.view().revision;
        assert!(updates.view().revision > first);
    }

    /// The whole look: nothing asked of the server while we host or watch,
    /// then its reply (the fixture, served on loopback) becomes the notice's
    /// event.
    #[tokio::test]
    async fn a_look_waits_while_busy_then_tells_the_ui() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Arc::new(AtomicUsize::new(0));
        let count = served.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                count.fetch_add(1, SeqCst);
                let mut request = vec![0; 4096];
                let _ = socket.read(&mut request).await;
                let answer = format!(
                    "HTTP/1.1 200 OK
Content-Length: {}
Connection: close

{FIXTURE}",
                    FIXTURE.len()
                );
                let _ = socket.write_all(answer.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        // Busy for the first three asks.
        let (actor, mut inbox) = mpsc::channel(4);
        let asked = Arc::new(AtomicUsize::new(0));
        let asks = asked.clone();
        tokio::spawn(async move {
            while let Some(input) = inbox.recv().await {
                if let rooms::Input::Busy(reply) = input {
                    let n = asks.fetch_add(1, SeqCst);
                    let _ = reply.send(n < 3);
                }
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let source = ReleaseSource {
            api: format!("http://{addr}/latest"),
            repository: "https://github.com/owner/name".into(),
            stand_in: true,
        };
        let updates = Arc::new(Updates::new(Some(source.clone()), dir.path().to_path_buf()));
        let (events, mut heard) = broadcast::channel(4);
        let pace = Pace {
            first: Duration::ZERO,
            wake: Duration::from_millis(20),
            retry_secs: 0,
        };
        let task = tokio::spawn(run_paced(updates.clone(), source, events, actor, pace));

        let event = tokio::time::timeout(Duration::from_secs(20), heard.recv())
            .await
            .expect("an event in time")
            .unwrap();
        let Event::Updates(view) = event else {
            panic!("{event:?}");
        };
        assert_eq!(
            view.new_version,
            Some(NewVersionView {
                version: "99.0.0".into(),
                wire_changed: true,
            })
        );
        assert_eq!(asked.load(SeqCst), 4, "asked until no longer busy");
        assert_eq!(
            served.load(SeqCst),
            1,
            "nothing asked of the server while busy"
        );
        // The stand-in looks once a start.
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("done after its look")
            .unwrap();
        // The stand-in keeps what it found in memory: the profile is the
        // installed app's too.
        assert_eq!(updates.view().new_version.unwrap().version, "99.0.0");
        assert!(!dir.path().join("updates.json").exists());
        updates.dismiss("99.0.0").unwrap();
        assert!(!dir.path().join("updates.json").exists());
    }

    #[test]
    fn the_switch_off_hides_the_notice() {
        let dir = tempfile::tempdir().unwrap();
        let updates = updates(dir.path());
        updates.heard(release("99.0.0", None), 1);
        store::save_app(
            dir.path(),
            store::AppSettings {
                updates: false,
                ..store::AppSettings::default()
            },
        )
        .unwrap();
        let view = updates.view();
        assert!(!view.on);
        assert_eq!(view.new_version, None);
    }
}
