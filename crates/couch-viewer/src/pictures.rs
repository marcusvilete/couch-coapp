//! Pictures on the board in the stream window (M27e, ADR 0013): those the
//! host sent and our own, by their id (the SHA-256 of their bytes), each
//! decoded with limits off the window's thread (`couch_media::picture`);
//! the clipboard's picture and dropped files, made what the board takes off
//! the window's thread too.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    ffi::CStr,
    path::Path,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
};

use couch_media::picture::{self, Decoded, MAX_INPUT_BYTES, Prepared};
use couch_proto::board::{MAX_BOARD_PICTURE_BYTES, MAX_BOARD_PICTURES, PictureId, Point};

struct Entry {
    bytes: Arc<Vec<u8>>,
    /// Decoded, while the board shows it; `None` meanwhile, and if it
    /// can't be. A picture no longer shown lets its image go (decoded, it
    /// can be 6.5 MB), and is decoded again if it comes back.
    image: Option<Arc<Decoded>>,
    /// Being decoded, or found not to decode: not asked again.
    asked: bool,
}

/// A picture made from the clipboard or a file, ready to paste where the
/// pointer was.
pub struct Ready {
    pub prepared: Prepared,
    pub at: Option<Point>,
}

/// A picture decoded off the window's thread, or not decodable.
type DecodedOne = (PictureId, Option<Arc<Decoded>>);
/// A picture made from the clipboard or a file off the window's thread.
type PreparedOne = Result<Ready, picture::PictureError>;

pub struct Pictures {
    entries: HashMap<PictureId, Entry>,
    /// Oldest first, for letting the unshown go.
    order: VecDeque<PictureId>,
    total: usize,
    decoded: (Sender<DecodedOne>, Receiver<DecodedOne>),
    /// The one thread decoding, started with the first picture.
    decoder: Option<Sender<(PictureId, Arc<Vec<u8>>)>>,
    prepared: (Sender<PreparedOne>, Receiver<PreparedOne>),
    /// Pictures being made, not yet pasted: one at a time is plenty.
    preparing: bool,
}

impl Default for Pictures {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            total: 0,
            decoded: channel(),
            decoder: None,
            prepared: channel(),
            preparing: false,
        }
    }
}

impl Pictures {
    /// Bytes kept at most, but for those the board shows: an undo may put
    /// back a picture erased a while ago.
    const KEEP: usize = 2 * MAX_BOARD_PICTURE_BYTES;

    /// Pictures kept at most, but for those the board shows.
    const KEEP_COUNT: usize = 2 * MAX_BOARD_PICTURES;

    /// A picture's bytes, checked against `id` by whoever gave them (the
    /// main process, or our own making): kept, and decoded off this thread
    /// once the board shows it ([`Self::sync`]). `shown` are the pictures the
    /// board shows, kept, but never more than the board holds: a host
    /// naming more can't make a window keep them all.
    pub fn add(&mut self, id: PictureId, bytes: Vec<u8>, shown: &BTreeSet<PictureId>) {
        if self.entries.contains_key(&id) {
            return;
        }
        self.total += bytes.len();
        self.entries.insert(
            id,
            Entry {
                bytes: Arc::new(bytes),
                image: None,
                asked: false,
            },
        );
        self.order.push_back(id);
        self.trim(id, shown);
        self.sync(shown);
    }

    /// The board shows `shown` now: their images are decoded (once), the
    /// rest let go of theirs.
    pub fn sync(&mut self, shown: &BTreeSet<PictureId>) {
        let shown = protected(shown);
        for (id, e) in &mut self.entries {
            if !shown.contains(id) {
                e.image = None;
                e.asked = false;
            } else if e.image.is_none() && !e.asked {
                e.asked = true;
                let decoder = self.decoder.get_or_insert_with(|| {
                    let (jobs, rx) = channel::<(PictureId, Arc<Vec<u8>>)>();
                    let tx = self.decoded.0.clone();
                    std::thread::spawn(move || {
                        while let Ok((id, bytes)) = rx.recv() {
                            let image = match picture::decode(&bytes) {
                                Ok(d) => Some(Arc::new(d)),
                                Err(e) => {
                                    tracing::warn!("a picture on the board can't be shown: {e}");
                                    None
                                }
                            };
                            if tx.send((id, image)).is_err() {
                                return;
                            }
                        }
                    });
                    jobs
                });
                let _ = decoder.send((*id, e.bytes.clone()));
            }
        }
    }

    fn trim(&mut self, keep: PictureId, shown: &BTreeSet<PictureId>) {
        if self.total <= Self::KEEP && self.order.len() <= Self::KEEP_COUNT {
            return;
        }
        let shown = protected(shown);
        let mut kept = VecDeque::new();
        while let Some(old) = self.order.pop_front() {
            let over = self.total > Self::KEEP || self.order.len() + kept.len() >= Self::KEEP_COUNT;
            if over && old != keep && !shown.contains(&old) {
                if let Some(e) = self.entries.remove(&old) {
                    self.total -= e.bytes.len();
                }
            } else {
                kept.push_back(old);
            }
        }
        self.order = kept;
    }

    pub fn bytes(&self, id: &PictureId) -> Option<Arc<Vec<u8>>> {
        self.entries.get(id).map(|e| e.bytes.clone())
    }

    pub fn image(&self, id: &PictureId) -> Option<&Arc<Decoded>> {
        self.entries.get(id).and_then(|e| e.image.as_ref())
    }

    /// Pictures decoded since: whether any was, to paint again.
    pub fn poll_decoded(&mut self) -> bool {
        let mut any = false;
        while let Ok((id, image)) = self.decoded.1.try_recv() {
            // Asked and not let go meanwhile: shown.
            if let Some(e) = self.entries.get_mut(&id)
                && e.asked
            {
                e.image = image;
                any = true;
            }
        }
        any
    }

    /// A picture from the clipboard or a file, made what the board takes off
    /// this thread; [`Self::poll_prepared`] hands it over. One at a time.
    pub fn prepare(&mut self, raw: Vec<u8>, at: Option<Point>) -> bool {
        if self.preparing {
            return false;
        }
        self.preparing = true;
        let tx = self.prepared.0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(picture::prepare(&raw).map(|prepared| Ready { prepared, at }));
        });
        true
    }

    /// The picture made, if one was.
    pub fn poll_prepared(&mut self) -> Option<Result<Ready, picture::PictureError>> {
        let ready = self.prepared.1.try_recv().ok()?;
        self.preparing = false;
        Some(ready)
    }
}

/// The pictures shown that are kept whatever else comes: no more than the
/// board holds.
fn protected(shown: &BTreeSet<PictureId>) -> BTreeSet<PictureId> {
    shown.iter().take(MAX_BOARD_PICTURES).copied().collect()
}

/// The clipboard's picture, if it holds one: a PNG, or a bitmap (a Windows
/// screenshot comes as one). On the window's thread: SDL's video is there.
pub fn clipboard_image() -> Option<Vec<u8>> {
    [c"image/png", c"image/bmp", c"image/jpeg"]
        .into_iter()
        .find_map(clipboard_data)
}

fn clipboard_data(mime: &CStr) -> Option<Vec<u8>> {
    use sdl3::sys::{
        clipboard::{SDL_GetClipboardData, SDL_HasClipboardData},
        stdinc::SDL_free,
    };
    // SAFETY: `mime` is a NUL-terminated string for the call's length; SDL
    // returns a buffer of `size` bytes it allocated, or null, which we copy
    // and give back with `SDL_free`, once.
    unsafe {
        if !SDL_HasClipboardData(mime.as_ptr()) {
            return None;
        }
        let mut size = 0usize;
        let data = SDL_GetClipboardData(mime.as_ptr(), &mut size);
        if data.is_null() {
            return None;
        }
        let bytes = (size > 0 && size <= MAX_INPUT_BYTES)
            .then(|| std::slice::from_raw_parts(data.cast::<u8>(), size).to_vec());
        SDL_free(data);
        bytes
    }
}

/// The first picture file copied to the clipboard (Explorer's or a file
/// manager's Ctrl+C on a file), by its name: pasted as a dropped one is.
pub fn clipboard_picture_file() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let files = couch_platform_win::clipboard::copied_files();
    #[cfg(not(windows))]
    let files = clipboard_data(c"text/uri-list")
        .map(|bytes| uri_list(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default();
    files.into_iter().find(|f| is_picture_name(f))
}

/// A file name a picture may have: PNG, JPEG or BMP.
fn is_picture_name(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["png", "jpg", "jpeg", "bmp"].contains(&e.to_ascii_lowercase().as_str()))
}

/// The local files a `text/uri-list` names (RFC 2483): `file://` URIs,
/// percent-decoded; comments and other URIs left out.
#[cfg_attr(windows, allow(dead_code, reason = "Linux's clipboard, and its test"))]
fn uri_list(list: &str) -> Vec<std::path::PathBuf> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        .filter_map(|rest| rest.strip_prefix("localhost").or(Some(rest)))
        .filter(|path| path.starts_with('/'))
        .filter_map(percent_decode)
        .map(std::path::PathBuf::from)
        .collect()
}

#[cfg_attr(windows, allow(dead_code, reason = "Linux's clipboard, and its test"))]
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A dropped file's bytes, if it is no larger than a picture may start.
pub fn read_dropped(path: &Path) -> Option<Vec<u8>> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 || len > MAX_INPUT_BYTES as u64 {
        return None;
    }
    std::fs::read(path).ok()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[test]
    fn a_uri_list_names_local_files_decoded() {
        let list = "# copied\r\nfile:///home/ana/Pictures/Mapa%20do%20jogo.png\r\nfile://localhost/tmp/a.JPG\r\nhttps://example.com/b.png\r\nfile:///tmp/%E2%9C%93.bmp\r\n";
        assert_eq!(
            uri_list(list),
            [
                PathBuf::from("/home/ana/Pictures/Mapa do jogo.png"),
                PathBuf::from("/tmp/a.JPG"),
                PathBuf::from("/tmp/\u{2713}.bmp"),
            ]
        );
        assert!(uri_list("file:///tmp/broken%2").is_empty());
    }

    #[test]
    fn only_picture_names_are_pasted() {
        assert!(is_picture_name(Path::new("a/b.PNG")));
        assert!(is_picture_name(Path::new("photo.jpeg")));
        assert!(!is_picture_name(Path::new("notes.txt")));
        assert!(!is_picture_name(Path::new("png")));
    }
}
