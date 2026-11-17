// The invite page (M7): turns https://<page>/j/#<invite> into the app's couchco://join#<invite>
// link. It reads only the part after "#", which browsers never send to the server, and
// makes no requests. The app checks the invite itself; this only refuses obvious junk and
// shows the room's name. Since M26c-3 it also offers the download, for the visitor's system
// first, and says when the owner's relay needs a code the link doesn't carry.
"use strict";

const LINK_PREFIX = "couchco://join#";
const MAX_CHARS = 4096;
// The payload is base32 of "CC", a version byte and the invite (couch_proto::invite):
// "inbq" is "CC" followed by the top bits of any version below 16.
const PAYLOAD = /^inbq[a-z2-7]+$/;
// The repository whose releases the download opens, as invite-page.yml writes it.
const REPOSITORY = /^[A-Za-z0-9-]+\/[A-Za-z0-9._-]+$/;
// iroh's public relays (couch_net::endpoint's `the_invite_page_knows_the_public_relays`
// checks this against iroh's list): they need no code.
const PUBLIC_RELAY = /\.relay\.n0\.iroh\.link$/;

function payloadOf(hash) {
  let text;
  try {
    // Chat apps may wrap the link or encode a space in it.
    text = decodeURIComponent(hash.replace(/^#/, ""));
  } catch {
    return null;
  }
  text = text.replace(/\s+/g, "").toLowerCase();
  return text.length <= MAX_CHARS && PAYLOAD.test(text) ? text : null;
}

function base32(text) {
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  const bytes = [];
  let bits = 0;
  let value = 0;
  for (const c of text) {
    value = (value << 5) | alphabet.indexOf(c);
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      bytes.push((value >> bits) & 0xff);
    }
  }
  return new Uint8Array(bytes);
}

// A reader over the invite's bytes, which throws past their end or on a varint too long.
function reader(bytes, at) {
  return {
    byte() {
      if (at >= bytes.length) throw new Error("short");
      return bytes[at++];
    },
    skip(count) {
      if (at + count > bytes.length) throw new Error("short");
      at += count;
    },
    // postcard's varint, for lengths: at most three bytes (no field here is longer than
    // 256), so a crafted one can't wrap around 32 bits into a small length.
    length() {
      let value = 0;
      for (let shift = 0; shift < 21; shift += 7) {
        const byte = this.byte();
        value |= (byte & 0x7f) << shift;
        if ((byte & 0x80) === 0) return value;
      }
      throw new Error("long");
    },
    // A varint of any size (up to a u64's ten bytes), only stepped over.
    skipVarint() {
      for (let i = 0; i < 10; i++) {
        if ((this.byte() & 0x80) === 0) return;
      }
      throw new Error("long");
    },
    text(length) {
      if (at + length > bytes.length) throw new Error("short");
      const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(at, at + length));
      at += length;
      return text;
    },
  };
}

// The invite's fields up to its address, as every version starts: after the magic and
// version (3 bytes), the owner's id (32) and the room's nonce (16), the name (postcard's
// varint length and UTF-8 text), the invite's token (32), when it expires (a varint), then
// the address, whose first field is the relay (an option: 0, or 1 and a string).
function fieldsOf(payload) {
  const bytes = base32(payload);
  const read = reader(bytes, 3 + 32 + 16);
  const nameLength = read.length();
  if (nameLength < 1 || nameLength > 96) throw new Error("name");
  const name = read.text(nameLength);
  read.skip(32);
  read.skipVarint();
  let relay = null;
  const tag = read.byte();
  if (tag === 1) {
    const length = read.length();
    if (length > 256) throw new Error("relay");
    relay = read.text(length);
  } else if (tag !== 0) {
    throw new Error("relay");
  }
  return { version: bytes[2], name, relay };
}

// The room's name, as the invite carries it. Anything unexpected gives no name rather
// than a wrong one.
function roomName(payload) {
  try {
    const { name } = fieldsOf(payload);
    return name.trim() ? name : null;
  } catch {
    return null;
  }
}

// The relay to ask for a code for: the invite names a relay that isn't one of iroh's
// public ones, and carries no code. Only version 3 can carry one (it is written only
// with it); a newer version may carry one some other way, so it gets no note. The code
// itself is never read. The relay's host name, or null.
function relayToAsk(payload) {
  try {
    const { version, relay } = fieldsOf(payload);
    if (relay === null || (version !== 1 && version !== 2)) return null;
    const host = new URL(relay).hostname.replace(/\.$/, "");
    return host && !PUBLIC_RELAY.test(host) ? host : null;
  } catch {
    return null;
  }
}

// The visitor's system, from what the browser says: "windows", "linux", "mac", "phone"
// (a phone or tablet: nothing to install there) or "other".
function systemOf(platform, userAgent, touchPoints) {
  const text = `${platform || ""} ${userAgent || ""}`;
  if (/Android|iPhone|iPad|iPod/i.test(text)) return "phone";
  // iPadOS says it's a Mac; a Mac has no touch screen.
  if (/Mac/i.test(text)) return touchPoints > 1 ? "phone" : "mac";
  if (/Windows/i.test(text)) return "windows";
  if (/CrOS|Chrome OS/i.test(text)) return "other";
  if (/Linux|X11/i.test(text)) return "linux";
  return "other";
}

// What the download part says for a system: the note above it, the main link and the
// file to pick on the release page, and the other system's link.
const DOWNLOADS = {
  windows: {
    first: "Download for Windows",
    pick: "On the release page, pick the file ending in x64-setup.exe and run it.",
    other: "Also for Linux: the AppImage",
  },
  linux: {
    first: "Download for Linux",
    pick: "On the release page, pick the file ending in .AppImage, make it executable and open it.",
    other: "Also for Windows: the setup file",
  },
};

function downloadsFor(system) {
  const note = {
    mac: "Couch Co-App isn't on macOS yet: it runs on Windows and Linux PCs.",
    phone: "Couch Co-App runs on Windows and Linux PCs: open this link on yours.",
  }[system];
  return { note: note || null, ...(DOWNLOADS[system] || DOWNLOADS.windows) };
}

function releasesOf(repository) {
  return REPOSITORY.test(repository || "") ? `https://github.com/${repository}/releases/latest` : null;
}

function showDownloads() {
  const meta = document.querySelector('meta[name="couch-releases"]');
  const releases = releasesOf(meta && meta.content);
  if (!releases) return;
  const nav = navigator.userAgentData;
  const system = systemOf(nav && nav.platform, navigator.userAgent, navigator.maxTouchPoints);
  const text = downloadsFor(system);
  // On a Mac or a phone, the note says where it runs instead.
  document.getElementById("get-it").textContent = text.note || "Couch Co-App is free, for Windows and Linux.";
  const first = document.getElementById("download-first");
  first.href = releases;
  first.textContent = text.first;
  document.getElementById("download-pick").textContent = text.pick;
  const other = document.getElementById("download-other");
  other.href = releases;
  other.textContent = text.other;
  document.getElementById("download-other-line").hidden = false;
  document.getElementById("downloads").hidden = false;
}

function show() {
  const payload = payloadOf(location.hash);
  document.getElementById("invite").hidden = !payload;
  document.getElementById("broken").hidden = !!payload;
  const title = document.getElementById("title");
  if (!payload) {
    title.textContent = "This invite doesn't work";
    return;
  }
  const link = LINK_PREFIX + payload;
  document.getElementById("open").href = link;
  const name = roomName(payload);
  title.textContent = name ? `You're invited to ${name}` : "You're invited to a room";
  const relay = relayToAsk(payload);
  document.getElementById("relay-note").hidden = !relay;
  document.getElementById("relay-name").textContent = relay || "";
  document.getElementById("copied").textContent = "";
  document.getElementById("copy").onclick = async () => {
    const status = document.getElementById("copied");
    try {
      await navigator.clipboard.writeText(link);
      status.textContent = "Copied.";
    } catch {
      status.textContent = "Couldn't copy: select the address bar and copy it instead.";
    }
  };
}

if (typeof document !== "undefined") {
  window.addEventListener("hashchange", show);
  show();
  showDownloads();
} else {
  // Under Node, for checking the decoding against invites the app made.
  module.exports = { payloadOf, roomName, relayToAsk, systemOf, downloadsFor, releasesOf };
}
