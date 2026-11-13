// What `shots.mjs` shoots beyond each scenario as it opens, and the no-jump
// check's views. Steps use what a person sees (roles and names), so they
// survive restyling.

const click = (name) => async (page) => page.getByRole("button", { name, exact: false }).first().click();
/** Right-clicks a member, opening the members drawer first in a narrow
 * window. */
/** The patch bay in the middle of the view: a mouse can't press what is
 * scrolled away. */
async function centreBay(page) {
  await page.locator(".bay").evaluate((el) => el.scrollIntoView({ block: "center" }));
  await page.waitForTimeout(150);
}

/** Drags `from` onto `to` with the mouse, letting go or not. */
async function dragTo(page, from, to, release) {
  await centreBay(page);
  const a = await page.locator(from).first().boundingBox();
  const b = await page.locator(to).first().boundingBox();
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(a.x + a.width / 2 + 30, a.y + a.height / 2 + 10, { steps: 4 });
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2, { steps: 12 });
  if (release) await page.mouse.up();
}

/** A menu must open: a right-click that opens nothing fails the run. */
const menuOpens = async (page) => page.getByRole("menu").first().waitFor({ timeout: 2000 });
const member = (name) => async (page) => {
  if ((await page.locator(".members-panel").count()) === 0) await page.getByRole("button", { name: "Members" }).click();
  await page.locator(".member", { hasText: name }).first().click({ button: "right" });
  await menuOpens(page);
};
/** The console's drawing panel, in view. */
const drawingPanel = async (page) => {
  await page.getByRole("region", { name: "Drawing on the stream" }).evaluate((el) => el.scrollIntoView({ block: "center" }));
  await page.waitForTimeout(150);
};
const settings = (section) => async (page) => {
  await page.getByRole("button", { name: "Settings (Ctrl+,)" }).click();
  if (section) await page.getByRole("button", { name: section, exact: true }).click();
};

/** Settings scrolled back to its top: typing in a field scrolls it into
 * view, which isn't a jump. */
const settingsTop = (page) => page.locator(".settings-content").evaluate((el) => el.scrollTo(0, 0));

/** A relay code as the engine writes it (M26a); the preview doesn't check
 * its checksum. */
const relayCode = (url, token, personal = false) =>
  `couch-relay-1.${Buffer.from(`${url}\n${token}\n${personal ? "personal" : ""}`).toString("base64url")}.5eed1e55`;

/** [name, scenario, steps]. A scenario of `sheet` shows the design
 * system's component sheet. */
export const SHOTS = [
  ["sheet", "sheet", null],
  ["sheet-menu", "sheet", async (page) => page.getByText("Right-click me").click({ button: "right" })],
  ["sheet-dialog", "sheet", click("Open a dialog")],
  // The host's board overlay while drawing (M27b), over a stand-in game.
  ["board-drawing", "board", null],
  ["board-drawing-fades", "board", async (page) => {
    // Twice round: fading, 4 s.
    await page.getByRole("button", { name: "Stays until erased" }).click();
    await page.getByRole("button", { name: "Fades away after 2 seconds" }).click();
  }],
  // A text box being typed in (M27c): the Text tool, its sizes, the caret.
  ["board-typing", "board", async (page) => {
    await page.getByRole("button", { name: "Text" }).click();
    // Above the toolbar at every size.
    const canvas = page.locator(".board-canvas");
    const box = await canvas.boundingBox();
    await canvas.click({ position: { x: Math.round(box.width * 0.45), y: Math.round(box.height * 0.42) } });
    await page.keyboard.type("Take the left door,");
    await page.keyboard.press("Enter");
    await page.keyboard.type("then jump");
  }],
  // Text selected (M27c): Shift+Ctrl+Left over the last word.
  ["board-typing-selected", "board", async (page) => {
    await page.getByRole("button", { name: "Text" }).click();
    const canvas = page.locator(".board-canvas");
    const box = await canvas.boundingBox();
    await canvas.click({ position: { x: Math.round(box.width * 0.45), y: Math.round(box.height * 0.42) } });
    await page.keyboard.type("Take the left door");
    await page.keyboard.press("Shift+Control+ArrowLeft");
  }],
  // The toolbar hidden to its pill (M27c). Last of the board's shots: the
  // page remembers it, and a look's and size's shots share their storage.
  ["board-tools-hidden", "board", click("Hide the drawing tools")],

  // Rooms
  ["dialog-add", "lobby", click("Create or join a room")],
  ["dialog-create", "first-run", click("Create a room")],
  ["dialog-join", "first-run", click("Join with an invite")],
  [
    "dialog-join-filled",
    "first-run",
    async (page) => {
      await click("Join with an invite")(page);
      await page.getByLabel("Invite link").fill("https://marcusvilete.github.io/couch-coapp/j/#v1.abc");
      await page.waitForTimeout(400);
    },
  ],
  ["dialog-invite", "owner-alone", click("Invite")],
  ["menu-room", "lobby", async (page) => {
    await click("Room options")(page);
    await menuOpens(page);
  }],
  ["menu-room-right-click", "lobby", async (page) => {
    await page.getByRole("button", { name: /^Sofá/ }).click({ button: "right" });
    await menuOpens(page);
  }],
  // In a room you own (in another's, a member has no menu: nothing to do).
  ["menu-member", "lobby", async (page) => {
    await page.getByRole("button", { name: /^Sofá/ }).click();
    await member("Caio")(page);
  }],
  [
    "confirm-ban",
    "lobby",
    async (page) => {
      await page.getByRole("button", { name: /^Sofá/ }).click();
      await member("Caio")(page);
      await page.getByRole("menuitem", { name: /Ban Caio/ }).click();
    },
  ],
  ["members-drawer", "lobby", async (page) => {
    if ((await page.locator(".members-panel").count()) === 0) await page.getByRole("button", { name: "Members" }).click();
  }],
  ["dialog-password", "restored-owner", click("Set a password")],
  ["dialog-accept", "handover-offered", click("Accept…")],

  // Sessions
  ["dialog-start", "owner-alone", click("Start a session")],
  ["dialog-start-remapper", "start-remapper", click("Start a session")],
  ["dialog-start-no-drivers", "start-no-drivers", click("Start a session")],
  // M16c-2: on a Linux PC run from afar, the keyboard seat is desktop
  // control's alone.
  ["dialog-start-remote-linux", "remote-linux-unlocked", click("Start a session on it…")],
  // M24: no seats at all, a session to watch together.
  ["dialog-start-together", "owner-alone", async (page) => {
    await click("Start a session")(page);
    await click("One controller less")(page);
    await page.getByRole("radio", { name: "Not for guests" }).click();
  }],
  ["dialog-stream", "hosting-idle", click("Start streaming…")],
  [
    "dialog-stream-admin",
    "hosting-idle",
    async (page) => {
      await click("Start streaming…")(page);
      await page.getByText("Task Manager").click();
    },
  ],
  [
    "menu-seat",
    "hosting-live",
    async (page) => {
      await centreBay(page);
      await page.locator(".bay-seat", { hasText: "Controller 4" }).click({ button: "right" });
      await menuOpens(page);
    },
  ],
  [
    "menu-watcher",
    "hosting-live",
    async (page) => {
      await centreBay(page);
      await page.locator(".bay-person.viewer", { hasText: "Dani" }).click({ button: "right" });
      await menuOpens(page);
    },
  ],
  // Drawing on the stream (M27a): the console's panel, and a drawer's menu.
  [
    "drawing-panel",
    "hosting-drawing",
    async (page) => {
      await page
        .getByRole("region", { name: "Drawing on the stream" })
        .evaluate((el) => el.scrollIntoView({ block: "center" }));
      await page.waitForTimeout(150);
    },
  ],
  // The board over the game (M27b): drawing there, and the key refused.
  ["drawing-over-game", "hosting-drawing-over-game", drawingPanel],
  ["drawing-refused", "hosting-drawing-refused", drawingPanel],
  // The host's own stream window (M27d): open and drawn in; on Linux, the
  // line saying it shows in a shared screen.
  ["own-window", "hosting-own-window", drawingPanel],
  ["own-window-linux", "linux-host", drawingPanel],
  [
    "drawing-menu",
    "hosting-drawing",
    async (page) => {
      await centreBay(page);
      await page.locator(".bay-person.viewer", { hasText: "Bia" }).click({ button: "right" });
      await menuOpens(page);
    },
  ],
  ["end-session", "hosting-live", async (page) => {
    await page.getByRole("button", { name: "Session options" }).click();
    await page.getByRole("menuitem", { name: "End session…" }).click();
  }],
  // The patch bay: dragging Dani onto Controller 4, held over it; then let go;
  // then Bia's plug pulled off Controller 3 into the open, held.
  ["patch-drag", "hosting-live", async (page) => dragTo(page, ".bay-plug.viewer >> text=Dani", ".bay-seat:has-text('Controller 4')", false)],
  ["patch-drop", "hosting-live", async (page) => {
    await dragTo(page, ".bay-plug.viewer >> text=Dani", ".bay-seat:has-text('Controller 4')", true);
    await page.waitForTimeout(400);
  }],
  // P13: on two seats with two controllers, asking for a third asks which
  // controller plays it.
  ["ask-with", "playing-two", async (page) => {
    await page.getByRole("button", { name: "Ask for Controller 3" }).click();
    await menuOpens(page);
  }],
  // P12: Bia's Xbox controller held over Controller 4, which her first
  // DualSense drives: the seat says they swap; then let go.
  ["patch-guest-pad-drag", "hosting-guest-controllers", async (page) =>
    dragTo(page, `[aria-label="Bia's Xbox Wireless Controller: drag onto a seat"]`, ".bay-seat:has-text('Controller 4')", false)],
  ["patch-guest-pad-drop", "hosting-guest-controllers", async (page) => {
    await dragTo(page, `[aria-label="Bia's Xbox Wireless Controller: drag onto a seat"]`, ".bay-seat:has-text('Controller 4')", true);
    await page.waitForTimeout(400);
  }],
  ["patch-keyboard-yours", "hosting-keyboard-yours", async (page) => centreBay(page)],
  ["patch-pull", "hosting-live", async (page) => {
    await centreBay(page);
    const plug = page.locator(".bay-seat:has-text('Controller 3') .bay-socket.plugged");
    const a = await plug.boundingBox();
    const bay = await page.locator(".bay").boundingBox();
    await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
    await page.mouse.down();
    await page.mouse.move(a.x - 40, a.y + 10, { steps: 4 });
    await page.mouse.move(bay.x + bay.width * 0.45, bay.y + 40, { steps: 8 });
  }],
  ["hosting-elsewhere", "hosting-live", async (page) => page.getByRole("button", { name: "Settings (Ctrl+,)" }).click()],

  // Settings
  ["settings-profile", "lobby", settings(null)],
  ["settings-appearance", "lobby", settings("Appearance")],
  ["settings-shortcuts", "lobby", settings("Shortcuts")],
  [
    "settings-shortcuts-capture",
    "lobby",
    async (page) => {
      await settings("Shortcuts")(page);
      await page.getByRole("button", { name: "Change" }).first().click();
    },
  ],
  ["settings-overlay", "lobby", settings("Overlay and sounds")],
  ["settings-overlay-linux", "linux-host", settings("Overlay and sounds")],
  // 0.26.1: the desktop keeps the host's keys it bound; only the stream window's change here.
  ["settings-shortcuts-linux", "linux-host", settings("Shortcuts")],
  // M16c-2: the GNOME extension beside the virtual devices, and naming the game's window.
  ["settings-drivers-gnome", "linux-keyboard-unnamed", settings("Drivers")],
  // P18: an older extension is updated, not installed; meanwhile no keyboard seat.
  ["settings-drivers-gnome-outdated", "linux-extension-outdated", settings("Drivers")],
  ["game-window-menu", "linux-keyboard-unnamed", click("Another…")],
  ["game-window-change", "linux-keyboard-named", click("Change…")],
  // M16c: a Linux host picks the sound, everything but us or one program.
  [
    "stream-picker-linux-sound",
    "linux-host",
    async (page) => {
      await click("Start streaming…")(page);
      await page.getByRole("combobox", { name: "Sound guests hear" }).click();
    },
  ],
  [
    "stream-picker-linux-program",
    "linux-host",
    async (page) => {
      await click("Start streaming…")(page);
      await page.getByRole("combobox", { name: "Sound guests hear" }).click();
      await page.getByRole("option", { name: "Hollow Knight" }).click();
    },
  ],
  // M16c-2: the picker on a Linux host whose session has a keyboard seat:
  // the desktop's dialog is offered, never refused for being a screen (the
  // VM run found it refused).
  ["stream-picker-linux-keyboard", "linux-keyboard-idle", click("Start streaming…")],
  ["settings-stream-window", "lobby", settings("Stream window")],
  ["settings-drawing", "lobby", settings("Drawing")],
  // M29: HDR converted for guests; Linux has no such section.
  ["settings-streaming", "lobby", settings("Streaming")],
  // M25: the home PC's remote play, a laptop trusted; and one set up anew.
  ["settings-remote-home", "remote-home", settings("Remote play")],
  ["settings-remote-new", "lobby", settings("Remote play")],
  // M25b: the home PC's settings while a device controls its desktop.
  ["settings-remote-desktop", "remote-home-desktop", settings("Remote play")],
  // M25c: the home PC's card for while you're away.
  ["settings-remote-away", "remote-away", settings("Remote play")],
  // M16c: a Linux home PC's: no notification area icon, the sharing dialog's note.
  ["settings-remote-linux", "linux-home-pc", settings("Remote play")],
  ["remote-unlock", "remote-locked", async (page) => page.getByRole("button", { name: "Unlock…" }).click()],
  ["settings-network", "lobby", settings("Network")],
  ["settings-network-release", "release-build", settings("Network")],
  // M26a: relays and their states, this network, adding, removing, the wizard.
  ["settings-network-relays", "network-relays", settings("Network")],
  ["settings-network-refused", "network-refused", settings("Network")],
  ["settings-network-offline", "network-offline", settings("Network")],
  ["settings-network-no-relay", "network-no-relay", settings("Network")],
  ["settings-network-udp", "network-udp-blocked", settings("Network")],
  ["settings-network-checking", "network-checking", settings("Network")],
  ["settings-network-public", "network-public", settings("Network")],
  ["network-code-read", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.getByLabel("Relay code").fill(relayCode("https://relay.caio-plays.net/", "s3cret", true));
    await page.getByText("relay.caio-plays.net").waitFor();
  }],
  ["network-code-held", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.getByLabel("Relay code").fill(relayCode("https://relay.friday-crew.org/", "new-one"));
    await page.getByText("adding gives it this new code").waitFor();
  }],
  ["network-code-damaged", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.getByLabel("Relay code").fill(relayCode("https://relay.caio-plays.net/", "s3cret").slice(0, 30));
    await page.getByText("incomplete").waitFor();
  }],
  ["network-code-refused", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.getByLabel("Relay code").fill(relayCode("https://relay.caio-plays.net/", "an-old-code"));
    await page.getByRole("button", { name: "Add", exact: true }).click();
    await page.getByText("refused this code").waitFor();
  }],
  ["network-code-down", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.getByLabel("Relay code").fill(relayCode("https://relay.down.example/", "s3cret"));
    await page.getByRole("button", { name: "Add", exact: true }).click();
    await page.getByRole("button", { name: "Save anyway" }).waitFor();
  }],
  ["network-type-address", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.getByRole("radio", { name: "Type its address" }).click();
  }],
  ["network-relay-menu", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.locator(".relay-row", { hasText: "relay.friday-crew.org" }).click({ button: "right" });
    await menuOpens(page);
  }],
  ["network-remove", "lobby", async (page) => {
    await settings("Network")(page);
    await page.getByRole("button", { name: "couchrelay.duckdns.org: options" }).click();
    await menuOpens(page);
    // The app's own relay stays: shown disabled, with why.
  }],
  ["network-remove-confirm", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.getByRole("button", { name: "relay.friday-crew.org: options" }).click();
    await page.getByRole("menuitem", { name: /Remove/ }).click();
    await page.getByText("Without it").first().waitFor();
  }],
  ["network-remove-unused", "network-relays", async (page) => {
    await settings("Network")(page);
    await page.locator(".relay-row", { hasText: "relay.bia-games.net" }).click({ button: "right" });
    await page.getByRole("menuitem", { name: /Remove/ }).click();
    await page.getByText("Nobody in your rooms").waitFor();
  }],
  // The strip's fix: Settings → Network, the code field asking for that relay's.
  ["strip-paste-code", "network-refused", click("Paste a new code")],
  ["wizard-create", "network-public-first-run", click("Create a room")],
  ["wizard-invite", "network-public", click("Invite")],
  ["wizard-code-read", "network-public-first-run", async (page) => {
    await click("Create a room")(page);
    await page.getByLabel("Relay code").fill(relayCode("https://relay.caio-plays.net/", "s3cret"));
    await page.getByText("relay.caio-plays.net").waitFor();
  }],
  // Answered: the room's own dialog follows.
  ["wizard-later", "network-public-first-run", async (page) => {
    await click("Create a room")(page);
    await click("Remind me later")(page);
    await page.getByRole("dialog", { name: /Create a room/ }).waitFor();
  }],
  ["settings-drivers", "lobby", settings("Drivers")],
  ["settings-backup", "lobby", settings("Backup")],
  ["settings-backup-empty", "first-run", settings("Backup")],
  ["settings-about", "lobby", settings("About")],
];

const SETTINGS = [
  "Appearance",
  "Shortcuts",
  "Overlay and sounds",
  "Stream window",
  "Network",
  "Drivers",
  "Backup",
  "About",
  "Profile",
];

/** [name, scenario, regions that must not move, [label, move][]]. */
export const STABLE = [
  [
    "rooms",
    "lobby",
    [".sidebar", ".room-header", ".members-panel"],
    [
      ["Sofá", async (page) => page.getByRole("button", { name: /^Sofá/ }).click()],
      ["Retro night", async (page) => page.getByRole("button", { name: /^Retro night/ }).click()],
      ["Friday games", async (page) => page.getByRole("button", { name: /^Friday games/ }).click()],
    ],
  ],
  [
    "settings",
    "lobby",
    [".sidebar", ".settings-nav", ".settings-content"],
    [
      ["open", settings(null)],
      ...SETTINGS.map((s) => [s, async (page) => page.getByRole("button", { name: s, exact: true }).click()]),
    ],
  ],
  // M26a: what a pasted code holds, a refusal and the other form appear in
  // reserved space: the relays above and the help below stay put.
  [
    "network add",
    "network-relays",
    [".relay-list", ".settings-content h3:last-of-type", ".settings-nav"],
    [
      ["open", settings("Network")],
      [
        "code",
        async (page) => {
          await page.getByLabel("Relay code").fill(relayCode("https://relay.caio-plays.net/", "s3cret", true));
          await page.getByText("relay.caio-plays.net").waitFor();
          await settingsTop(page);
        },
      ],
      [
        "refused",
        async (page) => {
          await page.getByLabel("Relay code").fill(relayCode("https://relay.caio-plays.net/", "an-old-code"));
          await page.getByRole("button", { name: "Add", exact: true }).click();
          await page.getByText("refused this code").waitFor();
          await settingsTop(page);
        },
      ],
      [
        "address",
        async (page) => {
          await page.getByRole("radio", { name: "Type its address" }).click();
          await settingsTop(page);
        },
      ],
    ],
  ],
  // The trouble strip comes and goes at the bottom: the room above stays.
  [
    "trouble strip",
    "network-refused",
    [".sidebar", ".room-header", ".members-panel"],
    [
      ["Sofá", async (page) => page.getByRole("button", { name: /^Sofá/ }).click()],
      ["Friday games", async (page) => page.getByRole("button", { name: /^Friday games/ }).click()],
    ],
  ],
  [
    "stream picker",
    "hosting-idle",
    [".dialog"],
    [
      ["open", click("Start streaming…")],
      ["Screens", async (page) => page.getByRole("tab", { name: /Screens/ }).click()],
      ["Windows", async (page) => page.getByRole("tab", { name: /Windows/ }).click()],
      ["pick", async (page) => page.getByText("Task Manager").click()],
    ],
  ],
  // M16c-2: naming the game's window changes its banner in place; the
  // requests and the patch bay below stay put. On KDE too, with its
  // corner's advice in both states.
  [
    "naming the game's window on KDE",
    "linux-keyboard-kde",
    [".console-top", ".console-banners"],
    [["name it", click("That's the game")]],
  ],
  [
    "naming the game's window",
    "linux-keyboard-unnamed",
    [".console-top", ".console-banners"],
    [
      ["name it", click("That's the game")],
      ["none", async (page) => {
        await click("Change…")(page);
        await page.getByRole("menuitem", { name: /None/ }).click();
      }],
    ],
  ],
  // M16c: a Linux host's picker, its one source, in the same dialog.
  [
    "stream picker on Linux",
    "linux-host",
    [".dialog"],
    [
      ["open", click("Start streaming…")],
      // M16c-1's fourth part: one program's sound.
      ["sound", async (page) => page.getByRole("combobox", { name: "Sound guests hear" }).click()],
      ["program", async (page) => page.getByRole("option", { name: "Hollow Knight" }).click()],
    ],
  ],
  [
    "start dialog",
    "owner-alone",
    [".dialog"],
    [
      ["open", click("Start a session")],
      ["more", click("One controller more")],
      ["more", click("One controller more")],
      ["keyboard", async (page) => page.getByRole("radio", { name: "A seat of their own" }).click()],
      ["none", click("One controller less")],
      // M24: down to no seats, a session to watch together.
      ["less", click("One controller less")],
      ["no controller", click("One controller less")],
      ["watch only", async (page) => page.getByRole("radio", { name: "Not for guests" }).click()],
    ],
  ],
  [
    // P11: the stream window's controls change in place; the card above
    // them never moves.
    "watch card",
    "watching",
    [".session-card", ".window-panel"],
    [
      ["full screen", click("Full screen")],
      ["bar off", async (page) => page.getByRole("switch", { name: "Bar" }).click()],
      ["back", click("Back to a window")],
      ["bar on", async (page) => page.getByRole("switch", { name: "Bar" }).click()],
    ],
  ],
  [
    // P12: a guest's controller moved to another seat of theirs, and back:
    // the cables move, the plugs and seats stay.
    "guest controllers",
    "hosting-guest-controllers",
    [".bay-people", ".bay-seats"],
    [
      ["swap", async (page) => dragTo(page, `[aria-label="Bia's Xbox Wireless Controller: drag onto a seat"]`, ".bay-seat:has-text('Controller 4')", true)],
      ["back", async (page) => dragTo(page, `[aria-label="Bia's DualSense Wireless Controller 1: drag onto a seat"]`, ".bay-seat:has-text('Controller 4')", true)],
    ],
  ],
  [
    "join dialog",
    "first-run",
    [".dialog"],
    [
      ["open", click("Join with an invite")],
      ["paste", async (page) => page.getByLabel("Invite link").fill("https://marcusvilete.github.io/couch-coapp/j/#v1.abc")],
      ["clear", async (page) => page.getByLabel("Invite link").fill("")],
    ],
  ],
];
