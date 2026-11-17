// Checks the invite page's reading of an invite against ones the app made:
//   node deploy/invite-page/check.js
// The payloads are couch_proto::invite's pinned test invites (`the_layout_is_pinned`,
// `the_handed_over_layout_is_pinned`, `the_invites_for_the_relay_note_are_pinned`).
"use strict";

const assert = require("node:assert");
const { payloadOf, roomName, relayToAsk, systemOf, downloadsFor, releasesOf } = require("./j/invite.js");

// Version 1, on relay.example.com, with no code.
const PINNED =
  "inbqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayarw2duoryhgorpf5zgk3dbpexgk6dbnvygyzjomnxw2lrpaa";

// Version 2 (M13), a room handed over: the founder follows the same fields.
const PINNED_V2 =
  "inbqeaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayarw2duoryhgorpf5zgk3dbpexgk6dbnvygyzjomnxw2lrpaaeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqs";

// Version 3 (M26b): the version 1 invite, carrying its relay's code.
const PINNED_V3 =
  "inbqgaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayarw2duoryhgorpf5zgk3dbpexgk6dbnvygyzjomnxw2lrpaaaay4ztmnzgk5bnorxwwzlo";

// Version 1 on iroh's public relay (euc1-1.relay.n0.iroh.link).
const PINNED_PUBLIC =
  "inbqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayasg2duoryhgorpf5sxkyzrfuys44tfnrqxsltogaxgs4tpnaxgy2lonmxc6aa";

// Version 1 with a direct address alone (a LAN), no relay.
const PINNED_LAN =
  "inbqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayaacagavaarjir5";

for (const payload of [PINNED, PINNED_V2, PINNED_V3, PINNED_PUBLIC, PINNED_LAN]) {
  assert.strictEqual(payloadOf("#" + payload), payload);
  assert.strictEqual(roomName(payload), "Sofá da Ana");
}
// What chat apps do to links: wrapped, upper-cased, a space encoded.
const half = PINNED.length / 2;
assert.strictEqual(payloadOf("#" + PINNED.slice(0, half).toUpperCase() + "%20" + PINNED.slice(half)), PINNED);
// Not invites.
for (const junk of ["", "#", "#hello", "#inbq!", "#%E0%A4%A", "#inbq" + "a".repeat(5000)]) {
  assert.strictEqual(payloadOf(junk), null, junk.slice(0, 20));
}
// Damaged: no name rather than a wrong one, and no note.
assert.strictEqual(roomName("inbqcaib"), null);
assert.strictEqual(relayToAsk("inbqcaib"), null);
assert.strictEqual(relayToAsk(PINNED.slice(0, -12)), null);
// A crafted name length of five bytes that 32 bits would wrap round to 5: refused.
const crafted = Buffer.concat([
  Buffer.from([0x43, 0x43, 1]),
  Buffer.alloc(48, 1),
  Buffer.from([0x85, 0x80, 0x80, 0x80, 0x10]),
  Buffer.from("hello"),
  Buffer.alloc(32, 4), // the token
  Buffer.from([0x00, 0x00]), // expires, no relay
]);
// Its honest twin, a one-byte length, reads.
const honest = Buffer.concat([crafted.subarray(0, 51), Buffer.from([0x05]), crafted.subarray(56)]);
const base32Of = (bytes) => {
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  let bits = "";
  for (const byte of bytes) bits += byte.toString(2).padStart(8, "0");
  let text = "";
  for (let i = 0; i < bits.length; i += 5) text += alphabet[parseInt(bits.slice(i, i + 5).padEnd(5, "0"), 2)];
  return text;
};
assert.strictEqual(roomName(base32Of(honest)), "hello");
assert.strictEqual(roomName(base32Of(crafted)), null);

// The relay note: an owner's relay with no code in the link, whether founder or not;
// never with the code, on iroh's public relays, or with no relay at all.
assert.strictEqual(relayToAsk(PINNED), "relay.example.com");
assert.strictEqual(relayToAsk(PINNED_V2), "relay.example.com");
assert.strictEqual(relayToAsk(PINNED_V3), null);
assert.strictEqual(relayToAsk(PINNED_PUBLIC), null);
assert.strictEqual(relayToAsk(PINNED_LAN), null);
// A version after 3 may carry a code another way: no note. The fifth character holds the
// version's low four bits ("c" is 1, "i" is 4), the rest unchanged.
assert.strictEqual(roomName("inbqi" + PINNED.slice(5)), "Sofá da Ana");
assert.strictEqual(relayToAsk("inbqi" + PINNED.slice(5)), null);

// The visitor's system, from what browsers say.
const UA = {
  windows: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36",
  linux: "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0",
  deck: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36",
  mac: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
  android: "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Mobile Safari/537.36",
  iphone: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1",
  chromebook: "Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36",
};
assert.strictEqual(systemOf(undefined, UA.windows, 0), "windows");
assert.strictEqual(systemOf("Windows", UA.windows, 0), "windows");
assert.strictEqual(systemOf(undefined, UA.linux, 0), "linux");
assert.strictEqual(systemOf("Linux", UA.deck, 10), "linux");
assert.strictEqual(systemOf(undefined, UA.mac, 0), "mac");
assert.strictEqual(systemOf("macOS", UA.mac, 0), "mac");
assert.strictEqual(systemOf(undefined, UA.mac, 5), "phone"); // an iPad
assert.strictEqual(systemOf("Android", UA.android, 5), "phone");
assert.strictEqual(systemOf(undefined, UA.iphone, 5), "phone");
assert.strictEqual(systemOf("Chrome OS", UA.chromebook, 0), "other");
assert.strictEqual(systemOf(undefined, "", 0), "other");

// The download: the visitor's system first, a note where there's nothing for it.
assert.match(downloadsFor("windows").first, /Windows/);
assert.match(downloadsFor("windows").pick, /setup\.exe/);
assert.match(downloadsFor("linux").first, /Linux/);
assert.match(downloadsFor("linux").pick, /\.AppImage/);
assert.strictEqual(downloadsFor("windows").note, null);
assert.match(downloadsFor("mac").note, /isn't on macOS yet/);
assert.match(downloadsFor("phone").note, /on yours/);
assert.match(downloadsFor("other").first, /Windows/);
// Only a repository as the workflow writes it; the placeholder gives no download.
assert.strictEqual(releasesOf("marcusvilete/couch-coapp"), "https://github.com/marcusvilete/couch-coapp/releases/latest");
for (const bad of ["", null, undefined, "couch-coapp", "a/b/c", "evil.example/x?y", "a/b#c", "javascript:x/y"]) {
  assert.strictEqual(releasesOf(bad), null, String(bad));
}
// The placeholder .github/workflows/invite-page.yml replaces when it publishes the page.
const page = require("node:fs").readFileSync(require("node:path").join(__dirname, "j", "index.html"), "utf8");
assert.ok(page.includes('<meta name="couch-releases" content="">'), "the releases placeholder");
console.log("invite page: ok");
