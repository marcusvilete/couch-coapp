//! Starting the app when the user signs in (M16c, ADR 0012: M25c's start
//! at sign-in, the Linux way): an XDG autostart entry,
//! `$XDG_CONFIG_HOME/autostart/couch-co-app.desktop`, naming the AppImage
//! with `--background` (no notification-area icon on Linux, so the window
//! opens).
//!
//! Only an AppImage starts so, and only its default profile (the engine
//! decides that). The entry is ours only with our key in it: another
//! program's file of that name is never read as set, nor removed, nor
//! rewritten. An AppImage's name carries its version, so a newer one
//! replacing it leaves the entry naming a file that's gone:
//! [`follow`] points it at the one running.

use std::{
    io,
    path::{Path, PathBuf},
};

const FILE: &str = "couch-co-app.desktop";
/// The key that makes an entry ours.
const OURS: &str = "X-CouchCoApp-Autostart=true";
/// The argument the app starts with at sign-in (the shell's own).
const BACKGROUND: &str = "--background";

/// The AppImage this process runs from, if it does: `$APPIMAGE`, when this
/// exe is inside the AppImage's mount (`$APPDIR`), so a variable inherited
/// from another AppImage counts for nothing.
pub fn own_appimage() -> Option<PathBuf> {
    let appimage = PathBuf::from(std::env::var_os("APPIMAGE")?);
    let appdir = PathBuf::from(std::env::var_os("APPDIR")?);
    let exe = std::env::current_exe().ok()?;
    (appimage.is_absolute() && exe.starts_with(&appdir)).then_some(appimage)
}

/// Whether the entry starts `appimage`.
pub fn is_set(appimage: &Path) -> io::Result<bool> {
    is_set_in(&dir()?, appimage)
}

/// Starts `appimage` at sign-in, or stops: our entry removed, whichever
/// AppImage it names.
pub fn set(appimage: &Path, on: bool) -> io::Result<()> {
    set_in(&dir()?, appimage, on)
}

/// Points our entry at `appimage` when the AppImage it names is gone (a
/// newer one replaced it). Returns whether it did.
pub fn follow(appimage: &Path) -> io::Result<bool> {
    follow_in(&dir()?, appimage)
}

/// The user's autostart folder.
fn dir() -> io::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home folder"))?;
    Ok(config.join("autostart"))
}

fn is_set_in(dir: &Path, appimage: &Path) -> io::Result<bool> {
    Ok(ours_in(dir)?.is_some_and(|named| same_file(&named, appimage)))
}

/// On, our entry names `appimage` (replacing ours for another). Off, ours
/// goes, whichever AppImage it names: the user wants Couch Co-App not to
/// start, and an older one beside this would otherwise go on starting.
fn set_in(dir: &Path, appimage: &Path, on: bool) -> io::Result<()> {
    let file = dir.join(FILE);
    let text = read(&file)?;
    if text.as_deref().is_some_and(|t| !has_our_key(t)) {
        if !on {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is another program's", file.display()),
        ));
    }
    if on {
        std::fs::create_dir_all(dir)?;
        return write(&file, &entry(appimage)?);
    }
    if text.is_some() {
        std::fs::remove_file(&file)?;
    }
    Ok(())
}

fn follow_in(dir: &Path, appimage: &Path) -> io::Result<bool> {
    let Some(named) = ours_in(dir)? else {
        return Ok(false);
    };
    if same_file(&named, appimage) || named.exists() {
        return Ok(false);
    }
    write(&dir.join(FILE), &entry(appimage)?)?;
    Ok(true)
}

/// What our entry names, if the file is ours.
fn ours_in(dir: &Path) -> io::Result<Option<PathBuf>> {
    Ok(read(&dir.join(FILE))?.and_then(|text| named(&text)))
}

pub(crate) fn read(file: &Path) -> io::Result<Option<String>> {
    match std::fs::read(file) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Written beside, then moved over: never half an entry.
pub(crate) fn write(file: &Path, text: &str) -> io::Result<()> {
    let temp = file.with_extension("desktop.new");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, file)
}

pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Our entry for `appimage`.
fn entry(appimage: &Path) -> io::Result<String> {
    let path = appimage
        .to_str()
        .filter(|p| appimage.is_absolute() && !p.chars().any(char::is_control))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the AppImage's path can't go in an autostart entry",
            )
        })?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Couch Co-App\n\
         Comment=Started at sign-in for remote play\n\
         Exec={} {BACKGROUND}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n\
         {OURS}\n",
        exec_quoted(path)
    ))
}

/// `path` as one quoted argument of an `Exec` key: quoting's escapes, `%`
/// doubled, then the string's own escape of each backslash (the Desktop
/// Entry Specification, "The Exec key").
pub(crate) fn exec_quoted(path: &str) -> String {
    let mut quoted = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted.replace('\\', "\\\\")
}

/// Whether an entry is ours: our key in it, whatever else was edited.
fn has_our_key(text: &str) -> bool {
    has_key(text, OURS)
}

pub(crate) fn has_key(text: &str, key: &str) -> bool {
    text.lines().any(|l| l.trim() == key)
}

/// The AppImage our entry names: `None` for an entry without our key, or
/// whose `Exec` isn't ours.
fn named(text: &str) -> Option<PathBuf> {
    named_by(text, OURS, &format!(" {BACKGROUND}"))
}

/// The program an entry with `key` in it starts: its `Exec`'s one quoted
/// path, followed by `rest` exactly.
pub(crate) fn named_by(text: &str, key: &str, rest: &str) -> Option<PathBuf> {
    if !has_key(text, key) {
        return None;
    }
    let exec = text
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("Exec="))?;
    // The string's escapes first, then quoting's.
    let mut unescaped = String::new();
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                '\\' => unescaped.push('\\'),
                's' => unescaped.push(' '),
                _ => return None,
            }
        } else {
            unescaped.push(c);
        }
    }
    let mut chars = unescaped.strip_prefix('"')?.chars();
    let mut path = String::new();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => path.push(chars.next()?),
            '%' => {
                if chars.next()? != '%' {
                    return None;
                }
                path.push('%');
            }
            c => path.push(c),
        }
    }
    (chars.as_str() == rest).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("couch-autostart-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn an_entry_names_its_appimage_whatever_its_path_holds() {
        for path in [
            "/home/ana/Apps/Couch-Co-App_0.26.0_abc.AppImage",
            "/home/ana/my games/Couch \"Co\" App $HOME `x` 100% \\ back.AppImage",
        ] {
            let text = entry(Path::new(path)).unwrap();
            assert_eq!(named(&text), Some(PathBuf::from(path)), "{text}");
        }
        // As the specification writes it.
        let text = entry(Path::new("/a/b \"c\".AppImage")).unwrap();
        assert!(
            text.contains("Exec=\"/a/b \\\\\"c\\\\\".AppImage\" --background\n"),
            "{text}"
        );
    }

    #[test]
    fn a_path_an_entry_cant_hold_is_refused() {
        assert!(entry(Path::new("relative.AppImage")).is_err());
        assert!(entry(Path::new("/a/new\nline.AppImage")).is_err());
    }

    #[test]
    fn only_our_entry_counts() {
        let text = entry(Path::new("/a/App.AppImage")).unwrap();
        assert!(named(&text.replace(OURS, "")).is_none(), "without our key");
        assert!(
            named(&text.replace(" --background", " --other")).is_none(),
            "another command"
        );
    }

    #[test]
    fn on_off_and_another_programs_file() {
        let dir = scratch("set");
        let this = Path::new("/apps/Couch-Co-App_0.26.0.AppImage");
        let other = Path::new("/apps/Couch-Co-App_0.25.0.AppImage");
        assert!(!is_set_in(&dir, this).unwrap());
        set_in(&dir, this, true).unwrap();
        assert!(is_set_in(&dir, this).unwrap());
        assert!(!is_set_in(&dir, other).unwrap());
        // Another AppImage on takes the entry over; off from any, it goes.
        set_in(&dir, other, true).unwrap();
        assert!(is_set_in(&dir, other).unwrap() && !is_set_in(&dir, this).unwrap());
        set_in(&dir, this, false).unwrap();
        assert!(!dir.join(FILE).exists());
        // Ours by its key, even with its command edited.
        std::fs::write(
            dir.join(FILE),
            format!("[Desktop Entry]\nExec=edited --flag\n{OURS}\n"),
        )
        .unwrap();
        assert!(!is_set_in(&dir, this).unwrap());
        set_in(&dir, this, true).unwrap();
        assert!(is_set_in(&dir, this).unwrap());
        std::fs::write(dir.join(FILE), format!("Exec=edited\n{OURS}\n")).unwrap();
        set_in(&dir, this, false).unwrap();
        assert!(!dir.join(FILE).exists());
        // A file of that name without our key is never ours to touch.
        std::fs::write(dir.join(FILE), "[Desktop Entry]\nExec=something\n").unwrap();
        assert!(set_in(&dir, this, true).is_err());
        set_in(&dir, this, false).unwrap();
        assert!(dir.join(FILE).exists());
        assert!(!follow_in(&dir, this).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_newer_appimage_takes_over_an_entry_naming_one_gone() {
        let dir = scratch("follow");
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("Couch-Co-App_0.25.0.AppImage");
        let new = dir.join("Couch-Co-App_0.26.0.AppImage");
        std::fs::write(&old, b"").unwrap();
        set_in(&dir, &old, true).unwrap();
        // The old one is still there: maybe the one the user runs.
        assert!(!follow_in(&dir, &new).unwrap());
        assert!(is_set_in(&dir, &old).unwrap());
        std::fs::remove_file(&old).unwrap();
        assert!(follow_in(&dir, &new).unwrap());
        assert!(is_set_in(&dir, &new).unwrap());
        assert!(!follow_in(&dir, &new).unwrap(), "once");
        // Off stays off.
        set_in(&dir, &new, false).unwrap();
        assert!(!follow_in(&dir, &new).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
