//! Images shown in Markdown buffers. Files are read and decoded on a
//! background thread, kept by path, and scaled once to the size they are
//! drawn at. Only local PNG files can be shown; anything else (remote URLs,
//! other formats, broken files) fails quietly and the text stays as written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::render::png::{self, Image};
use crate::terminal::WakeFn;

/// Files bigger than this are not read.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Decoded images are shrunk to this many pixels on their longest side, so
/// that scaling them on the UI thread later stays cheap.
const MAX_SIDE: usize = 1600;

enum Entry {
    Loading,
    Failed,
    Ready {
        image: Image,
        /// The last size it was drawn at.
        scaled: Option<Image>,
    },
}

/// Loads finished by the background threads: the path and the image, if it
/// could be read.
type Finished = Arc<Mutex<Vec<(PathBuf, Option<Image>)>>>;

#[derive(Default)]
pub struct Images {
    entries: HashMap<PathBuf, Entry>,
    /// Loads finished by the background threads, not yet picked up.
    finished: Finished,
}

impl Images {
    /// Size of the image at `path`, or `None` while it loads or when it
    /// cannot be shown. The first call starts loading it.
    pub fn size(&mut self, path: &Path, wake: Option<&WakeFn>) -> Option<(usize, usize)> {
        match self.entries.get(path) {
            Some(Entry::Ready { image, .. }) => return Some((image.width, image.height)),
            Some(_) => return None,
            None => {}
        }
        self.entries.insert(path.to_path_buf(), Entry::Loading);
        let finished = self.finished.clone();
        let wake = wake.cloned();
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let image = load(&path);
            finished.lock().unwrap_or_else(|e| e.into_inner()).push((path, image));
            if let Some(wake) = wake {
                wake();
            }
        });
        None
    }

    /// The image at `path` scaled to `width` x `height`, if it is loaded.
    pub fn scaled(&mut self, path: &Path, width: usize, height: usize) -> Option<&Image> {
        let Some(Entry::Ready { image, scaled }) = self.entries.get_mut(path) else { return None };
        if scaled.as_ref().is_none_or(|s| (s.width, s.height) != (width, height)) {
            *scaled = Some(resize(image, width, height));
        }
        scaled.as_ref()
    }

    /// Take in the images loaded since the last call. True when there were any.
    pub fn poll(&mut self) -> bool {
        let done: Vec<_> = std::mem::take(&mut *self.finished.lock().unwrap_or_else(|e| e.into_inner()));
        let any = !done.is_empty();
        for (path, image) in done {
            let entry = match image {
                Some(image) => Entry::Ready { image, scaled: None },
                None => Entry::Failed,
            };
            self.entries.insert(path, entry);
        }
        any
    }

    /// True while an image is still loading.
    pub fn loading(&self) -> bool {
        self.entries.values().any(|e| matches!(e, Entry::Loading))
    }

    /// Forget every image, so files changed on disk are read again.
    pub fn clear(&mut self) {
        self.entries.retain(|_, e| matches!(e, Entry::Loading));
    }
}

/// Read, decode and shrink one image. Runs on a background thread.
fn load(path: &Path) -> Option<Image> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let image = png::decode(&std::fs::read(path).ok()?)?;
    let longest = image.width.max(image.height);
    if longest <= MAX_SIDE {
        return Some(image);
    }
    let (w, h) = ((image.width * MAX_SIDE / longest).max(1), (image.height * MAX_SIDE / longest).max(1));
    Some(resize(&image, w, h))
}

/// Scale an image. Shrinking averages every source pixel a target pixel
/// covers (weighted by alpha, so transparent edges do not darken);
/// enlarging repeats pixels.
pub fn resize(image: &Image, width: usize, height: usize) -> Image {
    let (sw, sh) = (image.width, image.height);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let (y0, y1) = (y * sh / height, ((y + 1) * sh / height).max(y * sh / height + 1).min(sh));
        for x in 0..width {
            let (x0, x1) = (x * sw / width, ((x + 1) * sw / width).max(x * sw / width + 1).min(sw));
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = (sy * sw + sx) * 4;
                    let alpha = image.rgba[p + 3] as u64;
                    r += image.rgba[p] as u64 * alpha;
                    g += image.rgba[p + 1] as u64 * alpha;
                    b += image.rgba[p + 2] as u64 * alpha;
                    a += alpha;
                    n += 1;
                }
            }
            match (r.checked_div(a), g.checked_div(a), b.checked_div(a)) {
                (Some(r), Some(g), Some(b)) => rgba.extend_from_slice(&[r as u8, g as u8, b as u8, (a / n) as u8]),
                _ => rgba.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }
    Image { width, height, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_averages_and_keeps_transparency() {
        // 2x1: opaque white and fully transparent black, shrunk to 1x1.
        let image = Image { width: 2, height: 1, rgba: vec![255, 255, 255, 255, 0, 0, 0, 0] };
        let small = resize(&image, 1, 1);
        assert_eq!(small.rgba, vec![255, 255, 255, 127]);
        let big = resize(&image, 4, 2);
        assert_eq!((big.width, big.height, big.rgba.len()), (4, 2, 32));
        assert_eq!(&big.rgba[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn loads_in_the_background() {
        let mut images = Images::default();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon/icon-256.png");
        assert_eq!(images.size(&path, None), None, "not loaded yet");
        for _ in 0..500 {
            if images.poll() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(images.size(&path, None), Some((256, 256)));
        assert_eq!(images.scaled(&path, 32, 32).map(|i| i.rgba.len()), Some(32 * 32 * 4));
        assert_eq!(images.size(Path::new("/no/such/image.png"), None), None);
    }
}
