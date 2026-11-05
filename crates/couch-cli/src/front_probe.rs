//! `couch-cli front-probe` (Linux, M16c-2): what the desktop's helper says is
//! in front, live. It starts the helper as a session with a keyboard seat
//! does (KDE's KWin script, or a watch on our GNOME extension), then prints
//! the windows it lists and each change of the active window and the pointer
//! over it. Reads only: nothing is injected.

use std::time::{Duration, Instant};

use couch_platform_linux::front;

pub fn run(secs: u64) -> anyhow::Result<()> {
    match front::available() {
        Some(helper) => println!("helper: {helper:?}"),
        None => anyhow::bail!(
            "no helper: neither KDE's KWin nor Couch Co-App's GNOME extension (Settings → \
             Drivers) is running"
        ),
    }
    let dir = front::private_dir().ok_or_else(|| {
        anyhow::anyhow!("no folder of this user's alone: XDG_RUNTIME_DIR isn't set")
    })?;
    let watch =
        front::watch(&dir, &format!("probe-{}", std::process::id())).map_err(anyhow::Error::msg)?;
    let until = Instant::now() + Duration::from_secs(secs);
    let mut listed: Option<Vec<front::Window>> = None;
    let mut front_now: Option<(Option<isize>, bool)> = None;
    let (mut spoke, mut was_live) = (false, false);
    while Instant::now() < until {
        let live = front::live();
        if was_live && !live {
            println!("the helper went quiet: nothing counts as in front until it speaks again");
            front_now = None;
        }
        was_live = live;
        spoke |= live;
        if live {
            let windows = front::windows();
            if listed.as_ref() != Some(&windows) {
                println!("windows:");
                for w in &windows {
                    println!(
                        "  {:>3}  {:<28} pid {:<7} sizes {:?}  {}",
                        w.id, w.app, w.pid, w.sizes, w.title
                    );
                }
                listed = Some(windows);
            }
            // The active window, as one of ours if listed.
            let active = listed
                .iter()
                .flatten()
                .find(|w| front::focus(w.id).0)
                .map(|w| w.id);
            let pointer = active.is_some_and(|id| front::focus(id).1);
            if front_now != Some((active, pointer)) {
                match active {
                    Some(id) => println!(
                        "in front: {id}, pointer {}",
                        if pointer { "over it" } else { "elsewhere" }
                    ),
                    None => println!("in front: a window not listed (a dialog, a panel), or none"),
                }
                front_now = Some((active, pointer));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !spoke {
        anyhow::bail!("the helper never spoke");
    }
    drop(watch);
    // The helper stops on the portals' runtime: a moment for it.
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}
