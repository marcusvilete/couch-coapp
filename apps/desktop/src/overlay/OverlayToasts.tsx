// What the host overlay draws (M8): the requests waiting, the oldest with
// the keys that answer it, and short notes about the session. Shared by the
// overlay's page and the preview, which draws it in the main page.
import type { Toast } from "../model/overlay";

export function OverlayToasts(props: { toasts: Toast[] }) {
  return (
    <div className="overlay">
      {props.toasts.map((t) =>
        t.kind === "banner" ? (
          <div key={t.id} className="toast banner">
            <strong>{t.text}</strong>
            <span className="keys">
              {t.panic ? (
                <>
                  <kbd>{t.panic}</kbd> ends it
                </>
              ) : (
                "End it in the app"
              )}
            </span>
          </div>
        ) : t.kind === "request" ? (
          <div key={t.id} className={`toast request${t.first ? " first" : ""}`}>
            <strong>{t.text}</strong>
            {t.first && t.accept && t.decline ? (
              <span className="keys">
                <kbd>{t.accept}</kbd> accept · <kbd>{t.decline}</kbd> decline
              </span>
            ) : (
              <span className="keys">{t.first ? "Answer in the app" : "Waiting"}</span>
            )}
          </div>
        ) : (
          <div key={t.id} className="toast event">
            {t.text}
          </div>
        ),
      )}
    </div>
  );
}
