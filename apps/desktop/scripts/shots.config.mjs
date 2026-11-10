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
const settings = (section) => async (page) => {
  await page.getByRole("button", { name: "Settings (Ctrl+,)" }).click();
  if (section) await page.getByRole("button", { name: section, exact: true }).click();
};

/** [name, scenario, steps]. A scenario of `sheet` shows the design
 * system's component sheet. */
export const SHOTS = [
  ["sheet", "sheet", null],
  ["sheet-menu", "sheet", async (page) => page.getByText("Right-click me").click({ button: "right" })],
  ["sheet-dialog", "sheet", click("Open a dialog")],

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
