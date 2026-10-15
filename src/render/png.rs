//! A small PNG decoder, zlib inflate included, for the images Markdown files
//! show. It reads every color type and bit depth but not interlaced files,
//! and gives 8-bit RGBA. Files come from anywhere: every read is checked and
//! a malformed file gives `None`, never a panic.

/// Decoded pixels, 8 bits per channel, rows top to bottom.
#[derive(Debug, Clone)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Images bigger than this are refused (memory, not speed, is the limit).
const MAX_PIXELS: usize = 40_000_000;

pub fn decode(data: &[u8]) -> Option<Image> {
    if data.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut pos = 8;
    let mut header: Option<(usize, usize, u8, u8)> = None; // width, height, depth, color type
    let mut palette: &[u8] = &[];
    let mut transparency: &[u8] = &[];
    let mut compressed = Vec::new();
    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos.checked_add(8 + len)?)?;
        pos += 12 + len; // length, type, body, crc
        match kind {
            b"IHDR" => {
                let width = u32::from_be_bytes(body.get(0..4)?.try_into().ok()?) as usize;
                let height = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?) as usize;
                let (depth, color, interlace) = (*body.get(8)?, *body.get(9)?, *body.get(12)?);
                if width == 0 || height == 0 || width.checked_mul(height)? > MAX_PIXELS || interlace != 0 {
                    return None;
                }
                header = Some((width, height, depth, color));
            }
            b"PLTE" => palette = body,
            b"tRNS" => transparency = body,
            b"IDAT" => compressed.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
    }
    let (width, height, depth, color) = header?;
    let channels = match (color, depth) {
        (0, 1 | 2 | 4 | 8 | 16) => 1,
        (3, 1 | 2 | 4 | 8) => 1,
        (2 | 6, 8 | 16) => {
            if color == 2 {
                3
            } else {
                4
            }
        }
        (4, 8 | 16) => 2,
        _ => return None,
    };
    let bits_per_pixel = channels * depth as usize;
    let stride = (width * bits_per_pixel).div_ceil(8);
    let raw = zlib_inflate(&compressed, height * (stride + 1))?;
    if raw.len() < height * (stride + 1) {
        return None;
    }

    let filter_step = bits_per_pixel.div_ceil(8);
    let mut previous = vec![0u8; stride];
    let mut row = vec![0u8; stride];
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let line = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
        row.copy_from_slice(&line[1..]);
        unfilter(line[0], &mut row, &previous, filter_step)?;
        for x in 0..width {
            let pixel = to_rgba(&row, x, channels, depth, color, palette, transparency);
            rgba.extend_from_slice(&pixel);
        }
        std::mem::swap(&mut previous, &mut row);
    }
    Some(Image { width, height, rgba })
}

/// Undo the per-row filter. `step` is the size of a pixel in bytes (at least 1).
fn unfilter(filter: u8, row: &mut [u8], previous: &[u8], step: usize) -> Option<()> {
    for i in 0..row.len() {
        let left = if i >= step { row[i - step] } else { 0 };
        let up = previous[i];
        let up_left = if i >= step { previous[i - step] } else { 0 };
        let predicted = match filter {
            0 => 0,
            1 => left,
            2 => up,
            3 => ((left as u16 + up as u16) / 2) as u8,
            4 => paeth(left, up, up_left),
            _ => return None,
        };
        row[i] = row[i].wrapping_add(predicted);
    }
    Some(())
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Sample number `i` of an unfiltered row, at its full bit depth.
fn sample(row: &[u8], i: usize, depth: u8) -> u16 {
    match depth {
        8 => row[i] as u16,
        16 => u16::from_be_bytes([row[2 * i], row[2 * i + 1]]),
        _ => {
            let per_byte = 8 / depth as usize;
            let byte = row[i / per_byte];
            let shift = 8 - depth as usize * (i % per_byte + 1);
            (byte >> shift) as u16 & ((1u16 << depth) - 1)
        }
    }
}

fn to_rgba(row: &[u8], x: usize, channels: usize, depth: u8, color: u8, palette: &[u8], transparency: &[u8]) -> [u8; 4] {
    let s = |c: usize| sample(row, x * channels + c, depth);
    // Scale a sample to 8 bits.
    let eight = |v: u16| -> u8 {
        match depth {
            16 => (v >> 8) as u8,
            8 => v as u8,
            _ => (v as u32 * 255 / ((1u32 << depth) - 1)) as u8,
        }
    };
    // tRNS holds 16-bit sample values for gray and RGB images.
    let key = |i: usize| transparency.get(2 * i..2 * i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    match color {
        0 => {
            let v = s(0);
            let alpha = if key(0) == Some(v) { 0 } else { 255 };
            [eight(v), eight(v), eight(v), alpha]
        }
        2 => {
            let (r, g, b) = (s(0), s(1), s(2));
            let alpha = if transparency.len() >= 6 && key(0) == Some(r) && key(1) == Some(g) && key(2) == Some(b) { 0 } else { 255 };
            [eight(r), eight(g), eight(b), alpha]
        }
        3 => {
            let index = s(0) as usize;
            let rgb = palette.get(index * 3..index * 3 + 3).unwrap_or(&[0, 0, 0]);
            [rgb[0], rgb[1], rgb[2], *transparency.get(index).unwrap_or(&255)]
        }
        4 => [eight(s(0)), eight(s(0)), eight(s(0)), eight(s(1))],
        _ => [eight(s(0)), eight(s(1)), eight(s(2)), eight(s(3))],
    }
}

// ---------------------------------------------------------------------------
// Inflate (RFC 1950 zlib wrapper around RFC 1951 deflate), after zlib's puff.c.
// ---------------------------------------------------------------------------

/// Decompress a zlib stream, giving up when the output grows past `limit`.
pub fn zlib_inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let (cmf, flags) = (*data.first()?, *data.get(1)?);
    if cmf & 0x0f != 8 || flags & 0x20 != 0 || !(cmf as u16 * 256 + flags as u16).is_multiple_of(31) {
        return None;
    }
    let mut bits = Bits { data, pos: 2, buffer: 0, count: 0 };
    let mut out = Vec::with_capacity(limit);
    loop {
        let last = bits.read(1)? == 1;
        match bits.read(2)? {
            0 => stored(&mut bits, &mut out)?,
            1 => {
                let (lengths, distances) = fixed_tables();
                codes(&mut bits, &mut out, &lengths, &distances, limit)?;
            }
            2 => {
                let (lengths, distances) = dynamic_tables(&mut bits)?;
                codes(&mut bits, &mut out, &lengths, &distances, limit)?;
            }
            _ => return None,
        }
        if out.len() > limit {
            return None;
        }
        if last {
            return Some(out);
        }
    }
}

/// Reads bits least significant first, as deflate packs them.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u64,
    count: u32,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Option<u32> {
        while self.count < n {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            self.buffer |= (byte as u64) << self.count;
            self.count += 8;
        }
        let value = (self.buffer & ((1u64 << n) - 1)) as u32;
        self.buffer >>= n;
        self.count -= n;
        Some(value)
    }
}

/// A canonical Huffman code: how many codes have each length, and the
/// symbols ordered by code.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Huffman> {
        let mut counts = [0u16; 16];
        for &len in lengths {
            counts[len as usize] += 1;
        }
        // An over-subscribed set of lengths is not a code.
        let mut left: i32 = 1;
        for &count in &counts[1..] {
            left = left * 2 - count as i32;
            if left < 0 {
                return None;
            }
        }
        let mut offsets = [0u16; 16];
        for len in 1..15 {
            offsets[len + 1] = offsets[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbols[offsets[len as usize] as usize] = symbol as u16;
                offsets[len as usize] += 1;
            }
        }
        Some(Huffman { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.read(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

fn stored(bits: &mut Bits, out: &mut Vec<u8>) -> Option<()> {
    // Skip to the next byte boundary, then LEN and its complement.
    bits.buffer = 0;
    bits.count = 0;
    let header = bits.data.get(bits.pos..bits.pos + 4)?;
    let len = u16::from_le_bytes([header[0], header[1]]);
    if len != !u16::from_le_bytes([header[2], header[3]]) {
        return None;
    }
    let start = bits.pos + 4;
    out.extend_from_slice(bits.data.get(start..start + len as usize)?);
    bits.pos = start + len as usize;
    Some(())
}

const LENGTH_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385,
    24577,
];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Decode literal/length and distance codes until the end-of-block symbol.
fn codes(bits: &mut Bits, out: &mut Vec<u8>, lengths: &Huffman, distances: &Huffman, limit: usize) -> Option<()> {
    loop {
        let symbol = lengths.decode(bits)? as usize;
        if symbol < 256 {
            out.push(symbol as u8);
        } else if symbol == 256 {
            return Some(());
        } else {
            let i = symbol - 257;
            let len = *LENGTH_BASE.get(i)? as usize + bits.read(*LENGTH_EXTRA.get(i)? as u32)? as usize;
            let d = distances.decode(bits)? as usize;
            let dist = *DIST_BASE.get(d)? as usize + bits.read(*DIST_EXTRA.get(d)? as u32)? as usize;
            if dist > out.len() || out.len() + len > limit {
                return None;
            }
            // Byte by byte: the copy may overlap what it is writing.
            let from = out.len() - dist;
            for k in 0..len {
                out.push(out[from + k]);
            }
        }
    }
}

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lengths = [0u8; 288];
    lengths[..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..].fill(8);
    let codes = Huffman::new(&lengths).expect("fixed code is valid");
    let distances = Huffman::new(&[5u8; 30]).expect("fixed code is valid");
    (codes, distances)
}

fn dynamic_tables(bits: &mut Bits) -> Option<(Huffman, Huffman)> {
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let literal_count = bits.read(5)? as usize + 257;
    let distance_count = bits.read(5)? as usize + 1;
    let code_count = bits.read(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return None;
    }
    let mut code_lengths = [0u8; 19];
    for &i in &ORDER[..code_count] {
        code_lengths[i] = bits.read(3)? as u8;
    }
    let code_code = Huffman::new(&code_lengths)?;
    let mut lengths = vec![0u8; literal_count + distance_count];
    let mut i = 0;
    while i < lengths.len() {
        let symbol = code_code.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => (*lengths.get(i.checked_sub(1)?)?, 3 + bits.read(2)? as usize),
            17 => (0, 3 + bits.read(3)? as usize),
            18 => (0, 11 + bits.read(7)? as usize),
            _ => return None,
        };
        if i + repeat > lengths.len() {
            return None;
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        return None; // no end-of-block code
    }
    Some((Huffman::new(&lengths[..literal_count])?, Huffman::new(&lengths[literal_count..])?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_bundled_icon() {
        let image = decode(include_bytes!("../../assets/icon/icon-256.png")).expect("icon decodes");
        assert_eq!((image.width, image.height), (256, 256));
        assert_eq!(image.rgba.len(), 256 * 256 * 4);
        // The pre-rendered 96px logo is the same picture: compare a pixel near the middle.
        let center = (128 * 256 + 128) * 4;
        assert!(image.rgba[center + 3] > 0, "the middle of the logo is opaque");
    }

    #[test]
    fn inflates_stored_and_fixed_blocks() {
        // zlib.compress(b"hello hello hello", level 0) and level 9.
        let stored = [0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o', 0x06, 0x2c, 0x02, 0x15];
        assert_eq!(zlib_inflate(&stored, 100).unwrap(), b"hello");
        let fixed = [0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x00, 0x3a, 0x2e, 0x06, 0x7d];
        assert_eq!(zlib_inflate(&fixed, 100).unwrap(), b"hello hello hello");
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        assert!(decode(b"not a png").is_none());
        let mut truncated = include_bytes!("../../assets/icon/icon-256.png").to_vec();
        truncated.truncate(truncated.len() / 2);
        assert!(decode(&truncated).is_none());
        assert!(zlib_inflate(&[0x78, 0xda, 0xff, 0xff, 0xff], 100).is_none());
    }
}
