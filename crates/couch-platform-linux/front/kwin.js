// Couch Co-App (M16c-2, ADR 0012): tells the app which window is in front
// on KDE Plasma, so a guest's keys reach the game only while it is.
// Read-only: it reports, never acts. Written for one session, loaded over
// D-Bus, and unloaded when the session ends. The app takes these pushes
// only from KWin's own bus name, with this session's token, and only while
// they keep coming: what is in front is pushed every second too, and a
// script gone quiet counts for nothing.
//
// KWin 6's names, with KWin 5's (Plasma 5.27) where they differ.

const SERVICE = "@SERVICE@";
const TOKEN = "@TOKEN@";
const PATH = "/com/couchcoapp/Front";
const IFACE = "com.couchcoapp.Front";

function allWindows() {
    return workspace.windowList ? workspace.windowList() : workspace.clientList();
}

function activeWindow() {
    return workspace.windowList ? workspace.activeWindow : workspace.activeClient;
}

function on(signal6, signal5, handler) {
    const signal = workspace[signal6] || workspace[signal5];
    if (signal) {
        signal.connect(handler);
    }
}

function idOf(w) {
    return w ? String(w.internalId) : "";
}

function listed(w) {
    return w.normalWindow && !w.skipTaskbar;
}

function size(rect) {
    return [Math.round(rect.width), Math.round(rect.height)];
}

function windows(except) {
    return allWindows().filter(function (w) {
        return w !== except && listed(w);
    }).map(function (w) {
        return {
            id: idOf(w),
            title: String(w.caption),
            app: String(w.resourceClass),
            pid: w.pid > 0 ? w.pid : 0,
            sizes: [size(w.clientGeometry), size(w.frameGeometry)],
        };
    });
}

let sentWindows = "";
function pushWindows(except) {
    const json = JSON.stringify(windows(except));
    if (json !== sentWindows) {
        sentWindows = json;
        callDBus(SERVICE, PATH, IFACE, "Windows", TOKEN, json);
    }
}

function inside(rect, p) {
    return p.x >= rect.x && p.y >= rect.y && p.x < rect.x + rect.width && p.y < rect.y + rect.height;
}

// The window under the pointer: KWin 6's own answer, or the stacking
// order's topmost window there.
function windowUnder(p) {
    if (workspace.windowAt) {
        const under = workspace.windowAt(p, 1);
        return under.length > 0 ? under[0] : null;
    }
    const stack = workspace.stackingOrder;
    for (let i = stack.length - 1; i >= 0; i--) {
        const w = stack[i];
        if (w && !w.minimized && inside(w.frameGeometry, p)) {
            return w;
        }
    }
    return null;
}

// The pointer is over the active window's client area (never its title bar
// or frame), not under another window above it.
function pointerOver(active, p) {
    if (!active) {
        return false;
    }
    return inside(active.clientGeometry, p) && windowUnder(p) === active;
}

let sentFront = "";
function pushFront(always) {
    const active = activeWindow();
    const p = workspace.cursorPos;
    const front = { active: idOf(active), pointer: pointerOver(active, p), at: [p.x, p.y] };
    if (active) {
        // What a guest's pointer is kept in: the client area.
        const c = active.clientGeometry;
        front.area = [c.x, c.y, c.width, c.height];
    }
    const json = JSON.stringify(front);
    if (always || json !== sentFront) {
        sentFront = json;
        callDBus(SERVICE, PATH, IFACE, "Front", TOKEN, json);
    }
}

function follow(w) {
    w.captionChanged.connect(function () {
        pushWindows(null);
    });
    w.frameGeometryChanged.connect(function () {
        pushWindows(null);
        pushFront(false);
    });
}

allWindows().forEach(follow);
on("windowAdded", "clientAdded", function (w) {
    follow(w);
    pushWindows(null);
});
// The window going may still be in the list: left out.
on("windowRemoved", "clientRemoved", function (w) {
    pushWindows(w);
    pushFront(false);
});
on("windowActivated", "clientActivated", function () {
    pushFront(false);
});
// Where the pointer is goes at most every 8 ms while it moves: a guest's
// moves are cut from each position told (they slow near the game window's
// edges until the next).
const POINTER_MS = 8;
if (workspace.cursorPosChanged) {
    const soon = new QTimer();
    soon.singleShot = true;
    soon.interval = POINTER_MS;
    soon.timeout.connect(function () {
        pushFront(false);
    });
    workspace.cursorPosChanged.connect(function () {
        if (!soon.active) {
            soon.start();
        }
    });
} else {
    // No signal for the pointer: looked at often instead.
    const pointer = new QTimer();
    pointer.interval = 2 * POINTER_MS;
    pointer.timeout.connect(function () {
        pushFront(false);
    });
    pointer.start();
}

// Every second: the list if it changed, and what is in front always, so the
// app knows the script still speaks.
const beat = new QTimer();
beat.interval = 1000;
beat.timeout.connect(function () {
    pushWindows(null);
    pushFront(true);
});
beat.start();

pushWindows(null);
pushFront(true);
