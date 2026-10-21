// Checks the invite page's reading of an invite against one the app made:
//   node deploy/invite-page/check.js
// The payload is couch_proto::invite's pinned test invite (`the_layout_is_pinned`).
"use strict";

const assert = require("node:assert");
const { payloadOf, roomName } = require("./j/invite.js");

const PINNED =
  "inbqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayarw2duoryhgorpf5zgk3dbpexgk6dbnvygyzjomnxw2lrpaa";

// Version 2 (M13), a room handed over: the founder follows the same fields.
// `the_handed_over_layout_is_pinned`.
const PINNED_V2 =
  "inbqeaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayarw2duoryhgorpf5zgk3dbpexgk6dbnvygyzjomnxw2lrpaaeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqs";

assert.strictEqual(payloadOf("#" + PINNED), PINNED);
assert.strictEqual(roomName(PINNED), "Sofá da Ana");
assert.strictEqual(payloadOf("#" + PINNED_V2), PINNED_V2);
assert.strictEqual(roomName(PINNED_V2), "Sofá da Ana");
// What chat apps do to links: wrapped, upper-cased, a space encoded.
const half = PINNED.length / 2;
assert.strictEqual(payloadOf("#" + PINNED.slice(0, half).toUpperCase() + "%20" + PINNED.slice(half)), PINNED);
// Not invites.
for (const junk of ["", "#", "#hello", "#inbq!", "#%E0%A4%A", "#inbq" + "a".repeat(5000)]) {
  assert.strictEqual(payloadOf(junk), null, junk.slice(0, 20));
}
// Damaged: no name rather than a wrong one.
assert.strictEqual(roomName("inbqcaib"), null);
console.log("invite page: ok");
