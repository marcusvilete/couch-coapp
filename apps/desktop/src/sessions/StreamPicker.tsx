// Choosing what to stream, and how (M19): a window (best: only the game is
// shown, only its sound sent) or a whole screen, the frame rate and the
// quality, and an upload limit for those who need one.
import { AppWindow, Monitor, RefreshCw, ScreenShare, Search, ShieldAlert } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import type { SourceList } from "../bindings/SourceList";
import { errorText, request } from "../engine";
import { viaRemote, type RemoteTarget } from "../model/remote";
import {
  allowsKbm,
  EVERYTHING,
  FRAME_RATES,
  QUALITIES,
  screenNeedsWindow,
  soundChoices,
  uploadLimit,
  uploadLimitOk,
} from "../model/sessions";
import { KEYS, readStored, storedNumber, writeStored } from "../state/storage";
import { Button, IconButton } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Segmented, Select, TabPanel, Tabs } from "../ui/Controls";
import { Dialog } from "../ui/Dialog";
import { Badge, Skeleton } from "../ui/Feedback";
import { Check, Field, TextInput } from "../ui/Field";
import { rememberStream } from "./streamMemory";
import { useRun, type Notify } from "./useRun";
import "./StreamPicker.css";

type Picked = { kind: "window" | "screen"; id: string; title: string };

export function StreamPicker(props: {
  session: SessionView;
  onClose: () => void;
  onRoom: (room: RoomView) => void;
  notify: Notify;
  /** One of our PCs run from here (M25): its windows and screens. */
  remote?: RemoteTarget;
  /** This PC shares what its desktop's dialog offers (Linux, M16c): one
   * choice, made in that dialog when the stream starts. */
  shared?: boolean;
}) {
  const s = props.session;
  const { busy, run } = useRun(props.notify, props.onRoom, props.remote);
  const [sources, setSources] = useState<SourceList | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tab, setTab] = useState<"windows" | "screens">("windows");
  const [search, setSearch] = useState("");
  const [picked, setPicked] = useState<Picked | null>(null);
  const [fps, setFps] = useState(() => storedNumber(KEYS.streamFps, FRAME_RATES, 60));
  const [quality, setQuality] = useState(() =>
    storedNumber(
      KEYS.streamQuality,
      QUALITIES.map((q) => q.mbps),
      20,
    ),
  );
  const [limitUpload, setLimitUpload] = useState(false);
  const [upload, setUpload] = useState("50");
  const [error, setError] = useState<string | null>(null);
  // A Linux host's sound (M16c): `""` for everything but us, or a program.
  const [sound, setSound] = useState(() => readStored(KEYS.streamSound) ?? "");
  const [programs, setPrograms] = useState<string[] | null>(null);
  const [programsError, setProgramsError] = useState<string | null>(null);
  // Whether the desktop shares its last choice again without asking (Linux):
  // the desktops offer no way to undo that for this app, so it's undone here.
  const [remembered, setRemembered] = useState(false);
  const sharedChoice = (forget: boolean) =>
    request({ type: "sharedChoice", forget }, "sharedChoice")
      .then((r) => setRemembered(r.remembered))
      .catch(() => {});

  const loadPrograms = () => {
    setProgramsError(null);
    request({ type: "listSoundSources" }, "soundSources")
      .then((r) => setPrograms(r.programs))
      .catch((e) => {
        setPrograms([]);
        setProgramsError(errorText(e));
      });
  };
  const load = () => {
    setLoadError(null);
    request(viaRemote(props.remote, { type: "listSources" }), "sources")
      .then(setSources)
      .catch((e) => setLoadError(errorText(e)));
    if (props.shared) {
      loadPrograms();
      void sharedChoice(false);
    }
  };
  useEffect(load, []);
  // Its one source, picked at once.
  const sharedSource = props.shared ? (sources?.screens[0] ?? null) : null;
  useEffect(() => {
    if (sharedSource) setPicked({ kind: "screen", id: sharedSource.id, title: "Shared window or screen" });
    // Only a new source matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sharedSource?.id]);

  const live = s.stream.state === "live";
  const kbmSeats = allowsKbm(s);
  const uploadMbps = uploadLimit(limitUpload, upload);
  const isPicked = (kind: Picked["kind"], id: string) => picked?.kind === kind && picked.id === id;
  const pickedWindow = picked?.kind === "window" ? sources?.windows.find((w) => w.id === picked.id) : undefined;
  const pickedScreen = picked?.kind === "screen" ? sources?.screens.find((sc) => sc.id === picked.id) : undefined;
  const pickedElevated = pickedWindow?.elevated ?? false;
  // The screen's refresh rate caps the frames a capture gets. Windows
  // reports whole hertz, 59 for 59.94, hence the 1 Hz of room.
  const pickedRefresh = (pickedWindow ?? pickedScreen)?.refreshHz ?? null;
  const refreshTooLow = pickedRefresh != null && fps > pickedRefresh + 1;
  // From afar a screen goes with the keyboard seat: it is desktop control's
  // (M25b), and a guest holding it gets nothing in on a screen. So does the
  // desktop's dialog on Linux (M16c-2), whose share is listed as a screen:
  // the host names the game's window once it streams (`GameWindowBanner`).
  const screenWithKbm = screenNeedsWindow(kbmSeats, picked?.kind, {
    remote: props.remote != null,
    shared: props.shared === true,
  });
  const needle = search.trim().toLowerCase();
  const windows = (sources?.windows ?? []).filter(
    (w) => needle === "" || w.title.toLowerCase().includes(needle) || w.app.toLowerCase().includes(needle),
  );

  async function start(e: FormEvent) {
    e.preventDefault();
    const problem =
      (picked == null ? "Pick a window or a screen." : null) ??
      (screenWithKbm ? "This session has a keyboard-and-mouse seat: stream a window, not a whole screen." : null) ??
      (!uploadLimitOk(uploadMbps) ? "The upload limit is a whole number of Mbit/s, from 2 to 10000." : null);
    if (problem || picked == null) {
      setError(problem);
      return;
    }
    setError(null);
    writeStored(KEYS.streamFps, String(fps));
    writeStored(KEYS.streamQuality, String(quality));
    if (props.shared) writeStored(KEYS.streamSound, sound);
    rememberStream(picked.title, pickedElevated);
    const started = await run("stream", {
      type: "startStream",
      source: picked.kind === "window" ? { kind: "window", id: picked.id } : { kind: "screen", id: picked.id },
      name: picked.title,
      fps,
      max_mbps: quality,
      upload_mbps: uploadMbps,
      program: props.shared && sound !== "" ? sound : null,
    });
    if (started) props.onClose();
  }

  const qualityLabel = (mbps: number) => QUALITIES.find((q) => q.mbps === mbps)?.label ?? `${mbps} Mbit/s`;

  return (
    <Dialog
      title={live ? "Stream something else" : "What to stream"}
      description={
        props.shared
          ? "Your desktop asks which window or screen; pick here the sound guests hear."
          : "A window is best: only the game is shown, and only its sound is sent."
      }
      onClose={props.onClose}
      busy={busy != null}
      size="l"
      fixed
      pinned={
        <>
          {pickedElevated && (
            <Callout tone="warn" compact icon={<ShieldAlert />}>
              This program runs as administrator. Guests' keyboard and mouse can't reach it, and the panic key doesn't
              work while it is in front: switch to Couch Co-App and use Take back instead. Their gamepad still works.
            </Callout>
          )}
          {screenWithKbm && (
            <Callout tone="warn" compact>
              This session has a keyboard-and-mouse seat, which needs a window: pick the game's window, not a whole
              screen.
            </Callout>
          )}
          {refreshTooLow && (
            <Callout tone="warn" compact>
              Your screen runs at {pickedRefresh} Hz, so the stream can't go above {pickedRefresh} fps. Pick a lower
              frame rate, or raise the screen's refresh rate in Windows' display settings.
            </Callout>
          )}
          {error && (
            <p className="field-error" role="alert">
              {error}
            </p>
          )}
        </>
      }
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy != null}>
            Cancel
          </Button>
          <Button
            type="submit"
            form="stream-picker"
            variant="primary"
            disabled={busy != null || picked == null}
            labels={["Stream this instead", "Start streaming", "Starting…"]}
          >
            {busy === "stream" ? "Starting…" : live ? "Stream this instead" : "Start streaming"}
          </Button>
        </>
      }
    >
      <form id="stream-picker" className="picker" onSubmit={start}>
        <div className="picker-sources">
          {props.shared ? (
            <div className="picker-list">
              {loadError && <p className="field-error selectable">{loadError}</p>}
              {sources == null && !loadError && <SourceSkeletons rows={1} />}
              {sharedSource && (
                <div role="radiogroup" aria-label="What to share">
                  <label className="source picked">
                    <input type="radio" name="source" checked readOnly />
                    <span className="source-icon" aria-hidden="true">
                      <ScreenShare />
                    </span>
                    <span className="source-text">
                      <span className="source-title">A window or a screen</span>
                      <span className="source-detail">
                        {remembered
                          ? "Your last choice, shared without asking."
                          : "Your desktop asks which when the stream starts."}
                      </span>
                    </span>
                    {/* Always there, so it never comes and goes: each start
                        spends the desktop's remembered choice and the next
                        comes once it shares again. */}
                    <Button
                      size="sm"
                      disabled={busy != null || !remembered}
                      reason={remembered ? null : "Nothing remembered: your desktop asks which when the stream starts."}
                      onClick={() => void sharedChoice(true)}
                    >
                      Choose again
                    </Button>
                  </label>
                </div>
              )}
              <Field label="Sound guests hear">
                {(id) => (
                  <div className="picker-sound">
                    <Select
                      id={id}
                      value={sound === "" ? EVERYTHING : sound}
                      onChange={(v) => setSound(v === EVERYTHING ? "" : v)}
                      options={soundChoices(programs, sound)}
                      disabled={programs == null}
                    />
                    <IconButton label="Refresh what plays" onClick={loadPrograms} variant="secondary">
                      <RefreshCw />
                    </IconButton>
                  </div>
                )}
              </Field>
              {/* Reserved, so nothing below moves when it comes. */}
              <p className="picker-note selectable">
                {programsError ??
                  (sound === ""
                    ? "Everything your PC plays, except Couch Co-App's own sounds. For one program's alone, start it, then refresh."
                    : `Only ${sound}'s sound, whenever it plays.`)}
              </p>
            </div>
          ) : (
            <Tabs
              label="Sources"
              value={tab}
              onChange={setTab}
              tabs={[
                { value: "windows", label: `Windows${sources ? ` (${sources.windows.length})` : ""}` },
                { value: "screens", label: `Screens${sources ? ` (${sources.screens.length})` : ""}` },
              ]}
            >
              <div className="picker-tools">
                <span className="picker-search">
                  <Search aria-hidden="true" />
                  <TextInput
                    placeholder="Find a window"
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                    aria-label="Find a window"
                    disabled={tab !== "windows"}
                  />
                </span>
                <IconButton label="Refresh the list" onClick={load} variant="secondary">
                  <RefreshCw />
                </IconButton>
              </div>
              <TabPanel value="windows" className="picker-list">
                {loadError && <p className="field-error selectable">{loadError}</p>}
                {sources == null && !loadError && <SourceSkeletons />}
                {sources && windows.length === 0 && (
                  <p className="picker-empty">
                    {sources.windows.length === 0
                      ? "No windows found. Start the game, then refresh."
                      : "No window matches."}
                  </p>
                )}
                <div role="radiogroup" aria-label="Windows">
                  {windows.map((w) => (
                    <label key={w.id} className={`source${isPicked("window", w.id) ? " picked" : ""}`}>
                      <input
                        type="radio"
                        name="source"
                        checked={isPicked("window", w.id)}
                        onChange={() => setPicked({ kind: "window", id: w.id, title: w.title })}
                      />
                      <span className="source-icon" aria-hidden="true">
                        <AppWindow />
                      </span>
                      <span className="source-text">
                        <span className="source-title">{w.title}</span>
                        <span className="source-detail">{w.app}</span>
                      </span>
                      {w.elevated && (
                        <span title="Runs as administrator">
                          <Badge tone="warn">admin</Badge>
                        </span>
                      )}
                    </label>
                  ))}
                </div>
              </TabPanel>
              <TabPanel value="screens" className="picker-list">
                {sources == null && !loadError && <SourceSkeletons />}
                <div role="radiogroup" aria-label="Screens">
                  {sources?.screens.map((sc, i) => (
                    <label key={sc.id} className={`source${isPicked("screen", sc.id) ? " picked" : ""}`}>
                      <input
                        type="radio"
                        name="source"
                        checked={isPicked("screen", sc.id)}
                        onChange={() => setPicked({ kind: "screen", id: sc.id, title: `Screen ${i + 1}` })}
                      />
                      <span className="source-icon" aria-hidden="true">
                        <Monitor />
                      </span>
                      <span className="source-text">
                        <span className="source-title">
                          Whole screen {i + 1}
                          {sc.primary && " (main)"}
                        </span>
                        <span className="source-detail">
                          {sc.name}
                          {/* A Linux home PC's one source has no size (M16c). */}
                          {sc.width > 0 && sc.height > 0 && ` · ${sc.width}×${sc.height}`}
                          {sc.refreshHz != null && `, ${sc.refreshHz} Hz`}
                        </span>
                      </span>
                    </label>
                  ))}
                </div>
                <p className="picker-note">
                  For a screen, guests hear everything your PC plays, except Couch Co-App.
                  {kbmSeats && " A keyboard-and-mouse seat needs a window."}
                </p>
              </TabPanel>
            </Tabs>
          )}
        </div>

        <div className="picker-settings">
          <Field label="Frame rate">
            {() => (
              <Segmented
                label="Frame rate"
                value={fps}
                onChange={setFps}
                options={[...FRAME_RATES].reverse().map((f) => ({ value: f, label: `${f} fps` }))}
              />
            )}
          </Field>
          <Field label="Quality for each viewer">
            {(id) => (
              <Select
                id={id}
                value={quality}
                onChange={setQuality}
                options={QUALITIES.map((q) => {
                  const [rate, detail] = q.label.split(": ");
                  return { value: q.mbps, label: rate, detail };
                })}
              />
            )}
          </Field>
          <p className="picker-note">
            {fps === 120 && !refreshTooLow
              ? "120 fps doubles each viewer's encoding on your graphics card, and the same bitrate is spread over twice the frames. "
              : ""}
            Each viewer gets an encoder of their own and up to {qualityLabel(quality).split(":")[0]} of your upload: up to{" "}
            {quality * s.maxViewers} Mbit/s with {s.maxViewers} watching. A viewer on a slow connection gets less,
            without slowing the others.
          </p>
          <div className="picker-advanced">
            <Check checked={limitUpload} onChange={setLimitUpload}>
              Limit my total upload
            </Check>
            {limitUpload && (
              <Field label="Upload limit, Mbit/s (shared between viewers)">
                {(id) => <TextInput id={id} inputMode="numeric" value={upload} onChange={(e) => setUpload(e.target.value)} />}
              </Field>
            )}
          </div>
        </div>
      </form>
    </Dialog>
  );
}

/** `rows`: as many as will come (one for a Linux host's shared source), so
 * nothing below moves when they do. */
function SourceSkeletons(props: { rows?: number }) {
  return (
    <div className="source-skeletons">
      {Array.from({ length: props.rows ?? 4 }, (_, i) => (
        <div key={i} className="source">
          <span className="source-icon" />
          <span className="source-text">
            <Skeleton width={220} height={13} />
            <Skeleton width={120} height={10} />
          </span>
        </div>
      ))}
    </div>
  );
}
