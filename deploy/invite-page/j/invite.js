// The invite page (M7): turns https://<page>/j/#<invite> into the app's couchco://join#<invite>
// link. It reads only the part after "#", which browsers never send to the server, and
// makes no requests. The app checks the invite itself; this only refuses obvious junk and
// shows the room's name.
"use strict";

const LINK_PREFIX = "couchco://join#";
const MAX_CHARS = 4096;
// The payload is base32 of "CC", a version byte and the invite (couch_proto::invite):
// "inbq" is "CC" followed by the top bits of any version below 16.
const PAYLOAD = /^inbq[a-z2-7]+$/;

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

// The room's name, as the invite carries it: after the magic and version (3 bytes), the
// owner's id (32) and the room's nonce (16), postcard's varint length and UTF-8 text.
// Anything unexpected gives no name rather than a wrong one.
function roomName(payload) {
  try {
    const bytes = base32(payload);
    let at = 3 + 32 + 16;
    let length = 0;
    for (let shift = 0; shift < 35; shift += 7) {
      const byte = bytes[at++];
      if (byte === undefined) return null;
      length |= (byte & 0x7f) << shift;
      if ((byte & 0x80) === 0) break;
    }
    if (length < 1 || length > 96 || at + length > bytes.length) return null;
    const name = new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(at, at + length));
    return name.trim() ? name : null;
  } catch {
    return null;
  }
}

function show() {
  const payload = payloadOf(location.hash);
  document.getElementById("invite").hidden = !payload;
  document.getElementById("broken").hidden = !!payload;
  if (!payload) return;
  const link = LINK_PREFIX + payload;
  document.getElementById("open").href = link;
  const name = roomName(payload);
  const title = document.getElementById("title");
  title.textContent = name ? `You're invited to ${name}` : "You're invited to a room";
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
} else {
  // Under Node, for checking the decoding against an invite the app made.
  module.exports = { payloadOf, roomName };
}
