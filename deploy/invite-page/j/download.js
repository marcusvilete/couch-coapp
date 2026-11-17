// What the download offers, for the visitor's system (M26c-3). Shared by the invite page
// and the landing page (deploy/landing-page, which loads it from j/): one global,
// CouchDownload, in a browser; the same object under Node, for check.js.
"use strict";

const CouchDownload = (() => {
  // The repository whose releases the download opens, as the site's build writes it.
  const REPOSITORY = /^[A-Za-z0-9-]+\/[A-Za-z0-9._-]+$/;

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

  // What the download part says for a system: the main link, the file to pick on the
  // release page, and the other system's link.
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

  // A note where there's nothing for the system (a Mac, a phone), and its downloads,
  // Windows' for anything unknown.
  function downloadsFor(system) {
    const note = {
      mac: "Couch Co-App isn't on macOS yet: it runs on Windows and Linux PCs.",
      phone: "Couch Co-App runs on Windows and Linux PCs: open this on your PC.",
    }[system];
    return { note: note || null, ...(DOWNLOADS[system] || DOWNLOADS.windows) };
  }

  function releasesOf(repository) {
    return REPOSITORY.test(repository || "") ? `https://github.com/${repository}/releases/latest` : null;
  }

  // The visitor's own system, as this browser says it.
  function visitorSystem() {
    const nav = navigator.userAgentData;
    return systemOf(nav && nav.platform, navigator.userAgent, navigator.maxTouchPoints);
  }

  return { systemOf, downloadsFor, releasesOf, visitorSystem };
})();

if (typeof module !== "undefined") module.exports = CouchDownload;
