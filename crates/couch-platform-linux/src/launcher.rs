//! The app's menu entry (the author's choice, 2026-11-04): an AppImage
//! installs `$XDG_DATA_HOME/applications/com.couchcoapp.CouchCoApp.desktop`,
//! and its icon beside, when it starts.
//!
//! The portals need it: xdg-desktop-portal registers an app id only for an
//! installed entry of that name ("App info not found"), and without the
//! registration it refuses the host's keys (`portal`). GNOME finds a
//! window's icon through it too (by `StartupWMClass`, the window's class:
//! Tauri's is the binary's name).
//!
//! The entry is ours only with our key in it: another program's file of
//! that name is never rewritten. Ours naming an AppImage that's gone (a
//! newer one replaced it) is pointed at the one running; ours naming one
//! still there is left alone (it may be the one the user runs), the portals
//! needing only that the entry is there. The icon, named for our app id, is
//! written with an entry of ours (new or ours already), and never otherwise.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::{
    autostart::{exec_quoted, has_key, named_by, read, same_file, write},
    portal::APP_ID,
};

/// The key that makes an entry ours.
const OURS: &str = "X-CouchCoApp-Launcher=true";
/// The window's class, which GNOME matches entries by (Tauri's bundled
/// entry says the same).
const WINDOW_CLASS: &str = "couch-desktop";
/// The icon inside the AppImage's mount, as Tauri's bundler puts it.
const ICON_IN_APPIMAGE: &str = "couch-desktop.png";

/// Installs or updates our entry for the AppImage this process runs from;
/// nothing for a process that isn't one. Returns whether it wrote.
pub fn ensure() -> io::Result<bool> {
    let Some(appimage) = crate::autostart::own_appimage() else {
        return Ok(false);
    };
    let appdir = PathBuf::from(std::env::var_os("APPDIR").unwrap_or_default());
    let data = data_dir()?;
    ensure_in(
        &data.join("applications"),
        &data.join("icons"),
        &appimage,
        &appdir.join(ICON_IN_APPIMAGE),
    )
}

/// Whether an entry for our app id is installed, ours or not (the portals
/// need only that one is).
pub fn installed() -> bool {
    data_dir().is_ok_and(|data| {
        data.join("applications")
            .join(format!("{APP_ID}.desktop"))
            .exists()
    })
}

/// The user's data folder.
fn data_dir() -> io::Result<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|home| home.join(".local/share"))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home folder"))
}

fn ensure_in(apps: &Path, icons: &Path, appimage: &Path, icon: &Path) -> io::Result<bool> {
    let file = apps.join(format!("{APP_ID}.desktop"));
    if let Some(text) = read(&file)? {
        if !has_key(&text, OURS) {
            return Ok(false);
        }
        if let Some(named) = named_by(&text, OURS, "")
            && !same_file(&named, appimage)
            && named.exists()
        {
            return Ok(false);
        }
    }
    // The icon first: an entry naming a missing icon shows a blank one.
    let icon_file = icons.join(format!("{APP_ID}.png"));
    if let Ok(bytes) = std::fs::read(icon)
        && std::fs::read(&icon_file).ok().as_deref() != Some(bytes.as_slice())
    {
        std::fs::create_dir_all(icons)?;
        std::fs::write(&icon_file, bytes)?;
    }
    let text = entry(appimage, &icon_file)?;
    if read(&file)?.as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    std::fs::create_dir_all(apps)?;
    write(&file, &text)?;
    Ok(true)
}

/// Our entry for `appimage`, its icon at `icon`.
fn entry(appimage: &Path, icon: &Path) -> io::Result<String> {
    let (path, icon) = (fit(appimage)?, fit(icon)?);
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Couch Co-App\n\
         Comment=Play games on one couch, from afar\n\
         Exec={}\n\
         Icon={}\n\
         StartupWMClass={WINDOW_CLASS}\n\
         Categories=Game;Network;\n\
         Terminal=false\n\
         {OURS}\n",
        exec_quoted(path),
        // A string's own escape (the specification's), no quoting.
        icon.replace('\\', "\\\\"),
    ))
}

/// `path` as an entry can hold it: absolute, and no control characters.
fn fit(path: &Path) -> io::Result<&str> {
    path.to_str()
        .filter(|p| path.is_absolute() && !p.chars().any(char::is_control))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} can't go in a menu entry", path.display()),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("couch-launcher-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_entry_and_icon_are_installed_and_follow_a_newer_appimage() {
        let dir = scratch("follow");
        let (apps, icons) = (dir.join("applications"), dir.join("icons"));
        let icon = dir.join("couch-desktop.png");
        std::fs::write(&icon, b"png").unwrap();
        let old = dir.join("Couch-Co-App_0.26.0.AppImage");
        let new = dir.join("Couch-Co-App_0.26.1.AppImage");
        std::fs::write(&old, b"").unwrap();
        let file = apps.join(format!("{APP_ID}.desktop"));

        assert!(ensure_in(&apps, &icons, &old, &icon).unwrap());
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(named_by(&text, OURS, ""), Some(old.clone()));
        assert!(text.contains("StartupWMClass=couch-desktop\n"), "{text}");
        assert_eq!(
            std::fs::read(icons.join(format!("{APP_ID}.png"))).unwrap(),
            b"png"
        );
        assert!(!ensure_in(&apps, &icons, &old, &icon).unwrap(), "once");

        // Another AppImage still there: left alone.
        assert!(!ensure_in(&apps, &icons, &new, &icon).unwrap());
        // Gone: the running one takes the entry over.
        std::fs::remove_file(&old).unwrap();
        assert!(ensure_in(&apps, &icons, &new, &icon).unwrap());
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(named_by(&text, OURS, ""), Some(new));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn another_programs_file_is_never_rewritten() {
        let dir = scratch("other");
        let apps = dir.join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        let file = apps.join(format!("{APP_ID}.desktop"));
        std::fs::write(&file, "[Desktop Entry]\nExec=something\n").unwrap();
        let app = dir.join("Couch.AppImage");
        assert!(!ensure_in(&apps, &dir.join("icons"), &app, &dir.join("none.png")).unwrap());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "[Desktop Entry]\nExec=something\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn odd_paths_round_trip_and_unfit_ones_are_refused() {
        let icon = Path::new("/home/ana/.local/share/icons/x.png");
        let path = Path::new("/home/ana/my games/Couch \"Co\" $HOME 100%.AppImage");
        let text = entry(path, icon).unwrap();
        assert_eq!(named_by(&text, OURS, ""), Some(path.to_path_buf()));
        assert!(entry(Path::new("relative.AppImage"), icon).is_err());
        assert!(entry(Path::new("/a/new\nline.AppImage"), icon).is_err());
    }
}
