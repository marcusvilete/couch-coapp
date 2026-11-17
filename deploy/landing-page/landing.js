// The landing page's scripts: the download for the visitor's system (j/download.js,
// shared with the invite page), and the pickers. Without scripts the button opens the
// latest release as it is, and each picker shows every picture under its text.
"use strict";

(() => {
  const first = document.getElementById("download-first");
  const text = CouchDownload.downloadsFor(CouchDownload.visitorSystem());
  if (text.note) {
    const note = document.getElementById("download-note");
    note.textContent = text.note;
    note.hidden = false;
  }
  first.textContent = text.first;
  document.getElementById("download-pick").textContent = text.pick;
  document.getElementById("download-other").textContent = text.other;
  document.getElementById("download-other-line").hidden = false;
})();

// A picker: its items' titles on one side, the chosen one's picture on the other (on a
// wide screen; on a narrow one, every picture under its text, as without scripts).
for (const picker of document.querySelectorAll(".picker")) {
  const items = [...picker.querySelectorAll(".picker-item")];
  const choose = (chosen) => {
    for (const item of items) {
      const on = item === chosen;
      item.classList.toggle("chosen", on);
      item.querySelector(".picker-tab").setAttribute("aria-pressed", String(on));
    }
  };
  items.forEach((item, i) => {
    const tab = item.querySelector(".picker-tab");
    tab.addEventListener("click", () => choose(item));
    // Up and down move along the list, as in a list of choices.
    tab.addEventListener("keydown", (e) => {
      const next = { ArrowDown: 1, ArrowUp: -1 }[e.key];
      if (!next) return;
      e.preventDefault();
      const to = items[(i + next + items.length) % items.length];
      choose(to);
      to.querySelector(".picker-tab").focus();
    });
  });
  picker.style.setProperty("--items", String(items.length));
  picker.classList.add("on");
  choose(items[0]);
}

// A before/after picture: the range input over it moves the line between the two.
for (const compare of document.querySelectorAll(".compare")) {
  const range = compare.querySelector("input");
  const show = () => compare.style.setProperty("--split", `${range.value}%`);
  range.addEventListener("input", show);
  show();
}
