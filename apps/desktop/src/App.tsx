import { useEffect, useState } from "react";
import type { EngineInfo } from "./bindings/EngineInfo";
import { engineCommand, onEngineEvent } from "./engine";
import "./App.css";

type PingResult = { nonce: number; ms: number };

function App() {
  const [info, setInfo] = useState<EngineInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastPing, setLastPing] = useState<PingResult | null>(null);
  const [pongEvents, setPongEvents] = useState(0);

  useEffect(() => {
    engineCommand({ type: "getInfo" })
      .then((reply) => {
        if (reply.type === "info") setInfo(reply);
      })
      .catch((e) => setError(String(e)));

    const unlisten = onEngineEvent((event) => {
      if (event.type === "pong") setPongEvents((n) => n + 1);
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  async function ping() {
    const nonce = Math.floor(Math.random() * 1_000_000);
    const started = performance.now();
    try {
      const reply = await engineCommand({ type: "ping", nonce });
      if (reply.type === "pong" && reply.nonce === nonce) {
        setLastPing({ nonce, ms: performance.now() - started });
      }
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <main className="app">
      <header>
        <h1>Couch Co-App</h1>
        <p className="muted">Rooms and sessions arrive in milestones M4 and M5.</p>
      </header>

      {error && <p className="error">{error}</p>}

      <section className="card">
        <h2>Engine</h2>
        {info ? (
          <dl>
            <dt>Version</dt>
            <dd>{info.appVersion}</dd>
            <dt>Protocol</dt>
            <dd>{info.protocol}</dd>
            <dt>Profile</dt>
            <dd>{info.profile}</dd>
            <dt>Data folder</dt>
            <dd className="path">{info.dataDir}</dd>
          </dl>
        ) : (
          <p className="muted">Connecting…</p>
        )}
      </section>

      <section className="card">
        <h2>Round trip</h2>
        <button onClick={ping}>Ping the engine</button>
        <p>
          {lastPing
            ? `Pong ${lastPing.nonce} in ${lastPing.ms.toFixed(1)} ms`
            : "Not pinged yet."}
          {" · "}
          {pongEvents} pong event(s) received
        </p>
      </section>
    </main>
  );
}

export default App;
