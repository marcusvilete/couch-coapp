// Couch Co-App (M16c-2, ADR 0012): tells the app which window is in front
// on GNOME, so a guest's keys reach the game only while it is. Read-only:
// it reports, never acts. The app calls Watch(token) on the Shell's bus
// name while it hosts with a keyboard seat; this pushes to that caller
// alone (never a broadcast signal), and stops when it calls Unwatch or
// leaves the bus. The app takes these pushes only from the Shell's own bus
// name, with its token, and only while they keep coming: what is in front
// is pushed every second too. GNOME turns extensions off at the lock
// screen: this one says nothing is in front as it goes.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {getPointerWatcher} from 'resource:///org/gnome/shell/ui/pointerWatcher.js';

const PATH = '/com/couchcoapp/Front';
const IFACE = 'com.couchcoapp.Front';
const XML = `<node>
  <interface name="${IFACE}">
    <method name="Watch"><arg type="s" direction="in" name="token"/></method>
    <method name="Unwatch"/>
  </interface>
</node>`;
// At most this many apps (one per profile) watch at once.
const MAX_WATCHERS = 4;
// How often the Shell looks at the pointer while it moves (it stops while
// the user is idle). A guest's moves are cut from each position told (they
// slow near the game window's edges until the next).
const POINTER_MS = 16;
const NOTHING = JSON.stringify({active: '', pointer: false});

function listed(w) {
    return w.get_window_type() === Meta.WindowType.NORMAL && !w.is_skip_taskbar();
}

function size(rect) {
    return [rect.width, rect.height];
}

// The windows, `except` the one going (its actor may outlive it a moment).
function windows(except) {
    return global.get_window_actors()
        .map(a => a.get_meta_window())
        .filter(w => w && w !== except && listed(w))
        .map(w => ({
            id: String(w.get_id()),
            title: w.get_title() ?? '',
            app: w.get_wm_class() ?? '',
            pid: Math.max(w.get_pid(), 0),
            sizes: [size(w.get_frame_rect()), size(w.get_buffer_rect())],
        }));
}

function inside(rect, x, y) {
    return x >= rect.x && y >= rect.y && x < rect.x + rect.width && y < rect.y + rect.height;
}

// The window's client area: inside its frame, without the title bar Mutter
// draws (an app drawing its own title bar has it inside).
function clientArea(w) {
    return w.frame_rect_to_client_rect(w.get_frame_rect());
}

// The pointer is over the focused window's client area (never the title bar
// Mutter draws), and what a click there reaches is that window: not another
// window above it, nor the Shell's own UI over it (a notification and its
// buttons, the dock, a menu), which aren't windows.
function pointerOver(focused, x, y) {
    if (!focused)
        return false;
    if (!inside(clientArea(focused), x, y))
        return false;
    const windowActor = focused.get_compositor_private();
    const picked = global.stage.get_actor_at_pos(Clutter.PickMode.REACTIVE, x, y);
    return Boolean(windowActor && picked) &&
        (picked === windowActor || windowActor.contains(picked));
}

// The Shell's own UI has the keys (the run dialog, the overview, a menu, the
// lock screen), whatever window Mutter still calls focused.
function shellHasKeys() {
    if (Main.modalCount > 0 || Main.overview.visible || Main.sessionMode.isLocked)
        return true;
    const focus = global.stage.get_key_focus();
    return focus !== null && focus !== global.stage;
}

class Front {
    constructor() {
        this._watchers = new Map();
        this._ids = [];
        this._windowIds = new Map();
        this._sentWindows = '';
        this._sentFront = '';
        this._beat = 0;
    }

    WatchAsync(params, invocation) {
        const [token] = params;
        const sender = invocation.get_sender();
        if (!this._watchers.has(sender) && this._watchers.size >= MAX_WATCHERS) {
            invocation.return_dbus_error(`${IFACE}.Error.Busy`, 'too many watchers');
            return;
        }
        this._drop(sender);
        const nameId = Gio.bus_watch_name_on_connection(Gio.DBus.session, sender,
            Gio.BusNameWatcherFlags.NONE, null, () => this._drop(sender));
        this._watchers.set(sender, {token, nameId});
        invocation.return_value(null);
        if (this._watchers.size === 1)
            this._start();
        // The newcomer hears the state now.
        this._push(sender, 'Windows', JSON.stringify(windows(null)));
        this._push(sender, 'Front', this._front());
    }

    UnwatchAsync(params, invocation) {
        this._drop(invocation.get_sender());
        invocation.return_value(null);
    }

    // Turned off (the lock screen, or by the user): nothing is in front, told
    // to everyone watching before they are let go.
    destroy() {
        for (const sender of [...this._watchers.keys()]) {
            this._push(sender, 'Front', NOTHING);
            this._drop(sender);
        }
    }

    _drop(sender) {
        const watcher = this._watchers.get(sender);
        if (!watcher)
            return;
        Gio.bus_unwatch_name(watcher.nameId);
        this._watchers.delete(sender);
        if (this._watchers.size === 0)
            this._stop();
    }

    _start() {
        const display = global.display;
        this._ids.push([display, display.connect('window-created', (_d, w) => {
            this._follow(w);
            this._pushWindows(null);
        })]);
        this._ids.push([display, display.connect('notify::focus-window', () => this._pushFront(false))]);
        this._ids.push([display, display.connect('restacked', () => this._pushFront(false))]);
        this._ids.push([global.stage, global.stage.connect('notify::key-focus', () => this._pushFront(false))]);
        this._ids.push([Main.overview, Main.overview.connect('showing', () => this._pushFront(false))]);
        this._ids.push([Main.overview, Main.overview.connect('hidden', () => this._pushFront(false))]);
        for (const actor of global.get_window_actors()) {
            const w = actor.get_meta_window();
            if (w)
                this._follow(w);
        }
        this._pointer = getPointerWatcher().addWatch(POINTER_MS, () => this._pushFront(false));
        // Every second: the list if it changed, and what is in front always,
        // so the app knows this still speaks.
        this._beat = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 1, () => {
            this._pushWindows(null);
            this._pushFront(true);
            return GLib.SOURCE_CONTINUE;
        });
        this._sentWindows = '';
        this._sentFront = '';
    }

    _stop() {
        for (const [object, id] of this._ids)
            object.disconnect(id);
        this._ids = [];
        for (const [w, ids] of this._windowIds) {
            for (const id of ids)
                w.disconnect(id);
        }
        this._windowIds.clear();
        this._pointer?.remove();
        this._pointer = null;
        if (this._beat) {
            GLib.source_remove(this._beat);
            this._beat = 0;
        }
    }

    _follow(w) {
        if (this._windowIds.has(w))
            return;
        this._windowIds.set(w, [
            w.connect('notify::title', () => this._pushWindows(null)),
            w.connect('size-changed', () => {
                this._pushWindows(null);
                this._pushFront(false);
            }),
            // Its client area moved.
            w.connect('position-changed', () => this._pushFront(false)),
            w.connect('unmanaged', () => {
                const ids = this._windowIds.get(w) ?? [];
                for (const id of ids)
                    w.disconnect(id);
                this._windowIds.delete(w);
                this._pushWindows(w);
                this._pushFront(false);
            }),
        ]);
    }

    _front() {
        if (shellHasKeys())
            return NOTHING;
        const focused = global.display.focus_window;
        const [x, y] = global.get_pointer();
        const front = {
            active: focused ? String(focused.get_id()) : '',
            pointer: pointerOver(focused, x, y),
            at: [x, y],
        };
        if (focused) {
            // What a guest's pointer is kept in.
            const c = clientArea(focused);
            front.area = [c.x, c.y, c.width, c.height];
        }
        return JSON.stringify(front);
    }

    _pushWindows(except) {
        const json = JSON.stringify(windows(except));
        if (json === this._sentWindows)
            return;
        this._sentWindows = json;
        for (const sender of this._watchers.keys())
            this._push(sender, 'Windows', json);
    }

    _pushFront(always) {
        const json = this._front();
        if (!always && json === this._sentFront)
            return;
        this._sentFront = json;
        for (const sender of this._watchers.keys())
            this._push(sender, 'Front', json);
    }

    _push(sender, method, json) {
        const watcher = this._watchers.get(sender);
        if (!watcher)
            return;
        Gio.DBus.session.call(sender, PATH, IFACE, method,
            new GLib.Variant('(ss)', [watcher.token, json]), null,
            Gio.DBusCallFlags.NO_AUTO_START, 2000, null, null);
    }
}

export default class FrontExtension extends Extension {
    enable() {
        this._front = new Front();
        this._exported = Gio.DBusExportedObject.wrapJSObject(XML, this._front);
        this._exported.export(Gio.DBus.session, PATH);
    }

    disable() {
        this._exported.unexport();
        this._exported = null;
        this._front.destroy();
        this._front = null;
    }
}
