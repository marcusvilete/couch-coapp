<p align="center"><img src="docs/images/icon.svg" width="128" alt="Couch Co-App's icon: a couch whose backrest is a controller"></p>

<h1 align="center">Couch Co-App</h1>

### What is it, and why?

I miss the times when we sat on a couch and played games with our friends, passing the controller when someone died. This app is meant to emulate that, but remotely.

It's also a little experiment of mine to see how far we can go by just "vibecoding" something non-trivial. I purposely turned on Claude's "auto mode" and never looked at any code. It wrote the code, ran builds and tests, and committed the work, while I described features, assisted on debugging and tried things out. If you are curious, Claude did [document the journey](docs/architecture.md#status).

**[Visit the project's page](https://marcusvilete.github.io/couch-coapp/)** for details about its features.

### About trust and safety

By starting a session, you can let others in the room control your PC's controllers, and possibly keyboard and mouse. You should only use it with people you trust!

### Contributing

This project is mostly for personal use right now, so I won't be accepting pull requests. Bug reports, games that worked or didn't, and suggestions are welcome as [issues](https://github.com/marcusvilete/couch-coapp/issues).




> **DISCLAIMER: THE SMALL SECTION ABOVE WAS HUMAN-MADE. FROM THIS POINT ON, IT'S ALL CLAUDE DOING THE HEAVY-LIFTING.**
---



**Status:** in development, with most features in. Tested on Windows and Linux, but only on a handful of PCs so far: reports of how it runs on yours are welcome.

## Using it, building it, reading it

- **[Use it with your friends](docs/self-hosting.md):** install the [latest release](https://github.com/marcusvilete/couch-coapp/releases/latest), make a room and send its link. It works on iroh's free public relays; a small relay of your own can be added any time, step by step.
- **[Develop it](docs/development.md):** set up a PC to build, run and test the app.
- **[How it works](docs/architecture.md):** the design and the milestones, with the [wire protocol](docs/protocol.md) and the [decisions behind it](docs/adr/).

## Licence

Couch Co-App is free software under the **GNU General Public License, version 3** (GPL-3.0-only): see [LICENSE](LICENSE). You may use, study, share and change it. If you distribute a modified version, it must stay under the same licence, with its source available.

Bundled components keep their own licences:
- FFmpeg: LGPL v3, dynamically linked;
- SDL3: zlib;
- ViGEmBus: BSD-3-Clause;
- HidHide: MIT;
- the Rust crates and the front end's packages: mostly MIT and Apache-2.0, checked by `cargo deny`;
- in the Linux AppImage, Lucide's icon font (ISC) and the Ubuntu system libraries it carries, each named with its package and source.

`cargo xtask notices` gathers them all into `THIRD-PARTY-NOTICES.txt`. Every installer carries it, and the app shows it in Settings → About.
