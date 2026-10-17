//! A small JPEG decoder for the images Markdown files show. It reads
//! baseline and progressive Huffman-coded files with 8-bit samples (gray,
//! YCbCr, Adobe RGB/CMYK/YCCK), any chroma subsampling and restart markers,
//! and applies the EXIF orientation that phone photos rely on. Arithmetic
//! coding, 12-bit and lossless files are refused. Like the PNG decoder, every
//! read is checked: a malformed file gives `None`, never a panic.

use super::png::Image;

/// Images bigger than this are refused (memory, not speed, is the limit).
const MAX_PIXELS: usize = 40_000_000;

/// Position in a block, in the order the coefficients are coded, to the
/// position in the 8x8 grid (row by row).
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56,
    57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

struct Component {
    id: u8,
    /// Sampling factors.
    h: usize,
    v: usize,
    /// Quantization table number.
    tq: usize,
    /// Blocks per row and per column, padded to whole MCUs.
    blocks_wide: usize,
    blocks_high: usize,
    /// 64 coefficients per block, natural (row by row) order, not yet dequantized.
    coefs: Vec<i32>,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    components: Vec<Component>,
    hmax: usize,
    vmax: usize,
    mcus_wide: usize,
    mcus_high: usize,
}

pub fn decode(data: &[u8]) -> Option<Image> {
    if data.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut pos = 2;
    let mut quant: [[u16; 64]; 4] = [[0; 64]; 4];
    let mut dc_tables: [Option<Huffman>; 4] = Default::default();
    let mut ac_tables: [Option<Huffman>; 4] = Default::default();
    let mut restart_interval = 0;
    let mut frame: Option<Frame> = None;
    let mut adobe_transform: Option<u8> = None;
    let mut orientation = 1;
    let mut scans = 0;
    // Stop at the end of the data too: some files miss the end-of-image marker.
    while pos + 1 < data.len() {
        // Markers may be padded with any number of 0xff bytes.
        while data[pos] == 0xff && *data.get(pos + 1)? == 0xff {
            pos += 1;
        }
        if data[pos] != 0xff {
            return None;
        }
        let marker = *data.get(pos + 1)?;
        pos += 2;
        if marker == 0xd9 {
            break; // end of image
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue; // markers without a length
        }
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        let body = data.get(pos + 2..pos + len.max(2))?;
        pos += len.max(2);
        match marker {
            0xc0..=0xc2 => frame = Some(read_frame(body, marker == 0xc2)?),
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => return None, // lossless or arithmetic coding
            0xc4 => read_huffman_tables(body, &mut dc_tables, &mut ac_tables)?,
            0xdb => read_quant_tables(body, &mut quant)?,
            0xdd => restart_interval = u16::from_be_bytes([*body.first()?, *body.get(1)?]) as usize,
            0xe1 if body.starts_with(b"Exif\0\0") => orientation = exif_orientation(&body[6..]).unwrap_or(1),
            0xee if body.starts_with(b"Adobe") => adobe_transform = body.get(11).copied(),
            0xda => {
                let frame = frame.as_mut()?;
                let end = decode_scan(data, pos, body, frame, &dc_tables, &ac_tables, restart_interval)?;
                pos = end;
                scans += 1;
                if !frame.progressive {
                    break; // a baseline file has one scan with everything
                }
            }
            _ => {}
        }
    }
    let frame = frame?;
    if scans == 0 {
        return None;
    }
    let planes: Vec<Vec<u8>> = frame.components.iter().map(|c| component_plane(c, &quant[c.tq])).collect();
    let image = to_rgba(&frame, &planes, adobe_transform)?;
    Some(orient(image, orientation))
}

fn read_frame(body: &[u8], progressive: bool) -> Option<Frame> {
    if *body.first()? != 8 {
        return None; // 12-bit samples
    }
    let height = u16::from_be_bytes([*body.get(1)?, *body.get(2)?]) as usize;
    let width = u16::from_be_bytes([*body.get(3)?, *body.get(4)?]) as usize;
    let count = *body.get(5)? as usize;
    if width == 0 || height == 0 || width * height > MAX_PIXELS || !matches!(count, 1 | 3 | 4) {
        return None;
    }
    let mut components = Vec::new();
    for i in 0..count {
        let at = 6 + i * 3;
        let sampling = *body.get(at + 1)?;
        let (h, v) = ((sampling >> 4) as usize, (sampling & 15) as usize);
        let tq = *body.get(at + 2)? as usize;
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 {
            return None;
        }
        components.push(Component { id: *body.get(at)?, h, v, tq, blocks_wide: 0, blocks_high: 0, coefs: Vec::new() });
    }
    let hmax = components.iter().map(|c| c.h).max()?;
    let vmax = components.iter().map(|c| c.v).max()?;
    let mcus_wide = width.div_ceil(8 * hmax);
    let mcus_high = height.div_ceil(8 * vmax);
    for c in &mut components {
        c.blocks_wide = mcus_wide * c.h;
        c.blocks_high = mcus_high * c.v;
        c.coefs = vec![0; c.blocks_wide * c.blocks_high * 64];
    }
    Some(Frame { width, height, progressive, components, hmax, vmax, mcus_wide, mcus_high })
}

fn read_quant_tables(mut body: &[u8], quant: &mut [[u16; 64]; 4]) -> Option<()> {
    while let Some(&info) = body.first() {
        let (wide, id) = (info >> 4 != 0, (info & 15) as usize);
        let size = if wide { 128 } else { 64 };
        let values = body.get(1..1 + size)?;
        let table = quant.get_mut(id)?;
        for (k, &natural) in ZIGZAG.iter().enumerate() {
            table[natural] = if wide { u16::from_be_bytes([values[k * 2], values[k * 2 + 1]]) } else { values[k] as u16 };
        }
        body = &body[1 + size..];
    }
    Some(())
}

fn read_huffman_tables(mut body: &[u8], dc: &mut [Option<Huffman>; 4], ac: &mut [Option<Huffman>; 4]) -> Option<()> {
    while let Some(&info) = body.first() {
        let (class, id) = (info >> 4, (info & 15) as usize);
        let counts = body.get(1..17)?;
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        let values = body.get(17..17 + total)?;
        let table = Huffman::new(counts, values)?;
        match class {
            0 => *dc.get_mut(id)? = Some(table),
            1 => *ac.get_mut(id)? = Some(table),
            _ => return None,
        }
        body = &body[17 + total..];
    }
    Some(())
}

/// The orientation tag (1 to 8) from the TIFF data of an Exif segment.
fn exif_orientation(tiff: &[u8]) -> Option<u16> {
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let bytes = [*tiff.get(at)?, *tiff.get(at + 1)?];
        Some(if little { u16::from_le_bytes(bytes) } else { u16::from_be_bytes(bytes) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let bytes = [*tiff.get(at)?, *tiff.get(at + 1)?, *tiff.get(at + 2)?, *tiff.get(at + 3)?];
        Some(if little { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) })
    };
    let ifd = u32_at(4)? as usize;
    for i in 0..u16_at(ifd)? as usize {
        let entry = ifd + 2 + i * 12;
        if u16_at(entry)? == 0x0112 {
            return Some(u16_at(entry + 8)?).filter(|o| (1..=8).contains(o));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Entropy-coded data
// ---------------------------------------------------------------------------

/// Bits of the lookup table: codes this long or shorter decode in one step.
const FAST_BITS: u32 = 9;

struct Huffman {
    /// For every FAST_BITS-bit prefix: (code length, value), length 0 if longer.
    fast: Vec<(u8, u8)>,
    /// For longer codes, per length: the largest code (-1 when none), and the
    /// index in `values` of the first code minus that first code.
    max_code: [i32; 17],
    offset: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8], values: &[u8]) -> Option<Huffman> {
        let mut fast = vec![(0u8, 0u8); 1 << FAST_BITS];
        let mut max_code = [-1i32; 17];
        let mut offset = [0i32; 17];
        let mut code = 0i32;
        let mut index = 0usize;
        for len in 1..=16 {
            let count = counts[len - 1] as i32;
            offset[len] = index as i32 - code;
            for _ in 0..count {
                if len as u32 <= FAST_BITS {
                    let shift = FAST_BITS - len as u32;
                    let first = (code as usize) << shift;
                    let value = *values.get(index)?;
                    fast.get_mut(first..first + (1 << shift))?.fill((len as u8, value));
                }
                code += 1;
                index += 1;
            }
            if count > 0 {
                max_code[len] = code - 1;
            }
            if code > 1 << len {
                return None; // more codes than fit in this length
            }
            code <<= 1;
        }
        Some(Huffman { fast, max_code, offset, values: values.to_vec() })
    }
}

/// Reads bits from entropy-coded data, dropping the 0x00 after each 0xff.
/// At a marker it stops and returns zero bits.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn fill(&mut self) {
        while self.count <= 56 {
            let byte = match self.data.get(self.pos) {
                Some(0xff) => match self.data.get(self.pos + 1) {
                    Some(0x00) => {
                        self.pos += 2;
                        0xff
                    }
                    _ => 0, // a marker (or the end): stay in front of it
                },
                Some(&b) => {
                    self.pos += 1;
                    b
                }
                None => 0,
            };
            self.buffer |= (byte as u64) << (56 - self.count);
            self.count += 8;
        }
    }

    fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.count < n {
            self.fill();
        }
        let value = (self.buffer >> (64 - n)) as u32;
        self.buffer <<= n;
        self.count -= n;
        value
    }

    fn bit(&mut self) -> bool {
        self.bits(1) == 1
    }

    /// `n` bits as a signed value (the JPEG "receive and extend").
    fn signed(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let value = self.bits(n) as i32;
        if value < 1 << (n - 1) { value - (1 << n) + 1 } else { value }
    }

    fn decode(&mut self, table: &Huffman) -> Option<u8> {
        if self.count < 16 {
            self.fill();
        }
        let (len, value) = table.fast[(self.buffer >> (64 - FAST_BITS)) as usize];
        if len > 0 {
            self.buffer <<= len;
            self.count -= len as u32;
            return Some(value);
        }
        let mut code = 0i32;
        for len in 1..=16 {
            code = (code << 1) | self.bit() as i32;
            if code <= table.max_code[len] {
                return table.values.get((table.offset[len] + code) as usize).copied();
            }
        }
        None
    }

    /// After a restart interval: forget buffered bits and skip the RSTn marker.
    fn restart(&mut self) {
        self.buffer = 0;
        self.count = 0;
        if self.data.get(self.pos) == Some(&0xff) && self.data.get(self.pos + 1).is_some_and(|m| (0xd0..=0xd7).contains(m)) {
            self.pos += 2;
        }
    }
}

/// What one scan carries: the band of coefficients (`start..=end`) and, for
/// progressive files, the bit position (`low`) and whether it refines (`high`).
#[derive(Clone, Copy)]
struct Pass {
    progressive: bool,
    start: usize,
    end: usize,
    high: u32,
    low: u32,
}

/// Decode one scan whose entropy-coded data starts at `pos`; returns where
/// the next marker is.
fn decode_scan(
    data: &[u8],
    pos: usize,
    header: &[u8],
    frame: &mut Frame,
    dc_tables: &[Option<Huffman>; 4],
    ac_tables: &[Option<Huffman>; 4],
    restart_interval: usize,
) -> Option<usize> {
    let count = *header.first()? as usize;
    // For each component of the scan: its index in the frame and its tables.
    let mut scan: Vec<(usize, Option<&Huffman>, Option<&Huffman>)> = Vec::new();
    for i in 0..count {
        let id = *header.get(1 + i * 2)?;
        let tables = *header.get(2 + i * 2)?;
        let index = frame.components.iter().position(|c| c.id == id)?;
        scan.push((index, dc_tables.get((tables >> 4) as usize)?.as_ref(), ac_tables.get((tables & 15) as usize)?.as_ref()));
    }
    let at = 1 + count * 2;
    let (start, end, approx) = (*header.get(at)? as usize, *header.get(at + 1)? as usize, *header.get(at + 2)?);
    let pass = Pass { progressive: frame.progressive, start, end, high: (approx >> 4) as u32, low: (approx & 15) as u32 };
    if scan.is_empty() || start > end || end > 63 || pass.low > 13 {
        return None;
    }
    let mut bits = Bits { data, pos, buffer: 0, count: 0 };
    let mut predictions = vec![0i32; frame.components.len()];
    let mut eob_run = 0u32;
    let mut units_done = 0usize;
    // Before every unit but the first: is a restart marker due?
    let mut restart_due = || {
        let due = restart_interval > 0 && units_done > 0 && units_done.is_multiple_of(restart_interval);
        units_done += 1;
        due
    };
    if let [(index, dc, ac)] = scan[..] {
        // One component: its blocks one by one, only those inside the image.
        let (width, height, hmax, vmax) = (frame.width, frame.height, frame.hmax, frame.vmax);
        let component = &mut frame.components[index];
        let blocks_wide = (width * component.h).div_ceil(8 * hmax);
        let blocks_high = (height * component.v).div_ceil(8 * vmax);
        for by in 0..blocks_high {
            for bx in 0..blocks_wide {
                if restart_due() {
                    bits.restart();
                    predictions.fill(0);
                    eob_run = 0;
                }
                let block = by * component.blocks_wide + bx;
                let coefs = &mut component.coefs[block * 64..block * 64 + 64];
                decode_block(&mut bits, pass, coefs, dc, ac, &mut predictions[index], &mut eob_run)?;
            }
        }
    } else {
        // Several components: interleaved, a whole MCU at a time.
        for my in 0..frame.mcus_high {
            for mx in 0..frame.mcus_wide {
                if restart_due() {
                    bits.restart();
                    predictions.fill(0);
                    eob_run = 0;
                }
                for &(index, dc, ac) in &scan {
                    let component = &mut frame.components[index];
                    for y in 0..component.v {
                        for x in 0..component.h {
                            let block = (my * component.v + y) * component.blocks_wide + mx * component.h + x;
                            let coefs = &mut component.coefs[block * 64..block * 64 + 64];
                            decode_block(&mut bits, pass, coefs, dc, ac, &mut predictions[index], &mut eob_run)?;
                        }
                    }
                }
            }
        }
    }
    // The next marker (not a restart marker) ends the scan.
    let mut end = bits.pos;
    while end + 1 < data.len() && !(data[end] == 0xff && data[end + 1] != 0 && !(0xd0..=0xd7).contains(&data[end + 1])) {
        end += 1;
    }
    Some(end)
}

fn decode_block(
    bits: &mut Bits,
    pass: Pass,
    coefs: &mut [i32],
    dc: Option<&Huffman>,
    ac: Option<&Huffman>,
    prediction: &mut i32,
    eob_run: &mut u32,
) -> Option<()> {
    if !pass.progressive {
        decode_baseline(bits, coefs, dc?, ac?, prediction)
    } else if pass.start == 0 {
        // Progressive DC: the first scan gives the high bits, later ones one more bit each.
        if pass.high == 0 {
            let size = bits.decode(dc?)? as u32;
            *prediction = prediction.wrapping_add(bits.signed(size.min(16)));
            coefs[0] = prediction.wrapping_mul(1 << pass.low);
        } else if bits.bit() {
            coefs[0] |= 1 << pass.low;
        }
        Some(())
    } else if pass.high == 0 {
        decode_ac_first(bits, coefs, ac?, pass, eob_run)
    } else {
        decode_ac_refine(bits, coefs, ac?, pass, eob_run)
    }
}

fn decode_baseline(bits: &mut Bits, coefs: &mut [i32], dc: &Huffman, ac: &Huffman, prediction: &mut i32) -> Option<()> {
    let size = bits.decode(dc)? as u32;
    *prediction = prediction.wrapping_add(bits.signed(size.min(16)));
    coefs[0] = *prediction;
    let mut k = 1;
    while k < 64 {
        let symbol = bits.decode(ac)?;
        let (run, size) = ((symbol >> 4) as usize, (symbol & 15) as u32);
        if size == 0 {
            if run != 15 {
                break; // end of block
            }
            k += 16; // sixteen zeros
            continue;
        }
        k += run;
        coefs[*ZIGZAG.get(k)?] = bits.signed(size);
        k += 1;
    }
    Some(())
}

/// First progressive scan of an AC band. `eob_run` counts the blocks left
/// whose band is all zeros.
fn decode_ac_first(bits: &mut Bits, coefs: &mut [i32], ac: &Huffman, pass: Pass, eob_run: &mut u32) -> Option<()> {
    if *eob_run > 0 {
        *eob_run -= 1;
        return Some(());
    }
    let mut k = pass.start;
    while k <= pass.end {
        let symbol = bits.decode(ac)?;
        let (run, size) = ((symbol >> 4) as u32, (symbol & 15) as u32);
        if size == 0 {
            if run < 15 {
                *eob_run = (1 << run) - 1 + bits.bits(run);
                break;
            }
            k += 16;
            continue;
        }
        k += run as usize;
        coefs[*ZIGZAG.get(k)?] = bits.signed(size) * (1 << pass.low);
        k += 1;
    }
    Some(())
}

/// Later progressive scan of an AC band: one more bit for the coefficients
/// already known, and new coefficients of size one.
fn decode_ac_refine(bits: &mut Bits, coefs: &mut [i32], ac: &Huffman, pass: Pass, eob_run: &mut u32) -> Option<()> {
    let bit = 1 << pass.low;
    let mut k = pass.start;
    if *eob_run > 0 {
        *eob_run -= 1;
        for &natural in &ZIGZAG[pass.start..=pass.end] {
            refine(bits, &mut coefs[natural], bit);
        }
        return Some(());
    }
    while k <= pass.end {
        let symbol = bits.decode(ac)?;
        let (mut run, size) = ((symbol >> 4) as u32, (symbol & 15) as u32);
        let mut value = 0;
        if size == 0 {
            if run < 15 {
                // End of band: the rest of this block only gets refined.
                *eob_run = (1 << run) - 1 + bits.bits(run);
                run = 64;
            }
        } else if size == 1 {
            value = if bits.bit() { bit } else { -bit };
        } else {
            return None;
        }
        // Skip `run` zero coefficients (refining the others on the way),
        // then place the new one.
        while k <= pass.end {
            let coef = &mut coefs[ZIGZAG[k]];
            k += 1;
            if *coef != 0 {
                refine(bits, coef, bit);
            } else if run == 0 {
                *coef = value;
                break;
            } else {
                run -= 1;
            }
        }
    }
    Some(())
}

fn refine(bits: &mut Bits, coef: &mut i32, bit: i32) {
    if *coef != 0 && bits.bit() && *coef & bit == 0 {
        *coef += if *coef > 0 { bit } else { -bit };
    }
}

// ---------------------------------------------------------------------------
// Pixels
// ---------------------------------------------------------------------------

/// Dequantize and inverse-transform every block of a component into a plane
/// of samples, `blocks_wide * 8` samples per row.
fn component_plane(component: &Component, quant: &[u16; 64]) -> Vec<u8> {
    // table[x][u]: the weight of frequency u at position x.
    let mut table = [[0f32; 8]; 8];
    for (x, row) in table.iter_mut().enumerate() {
        for (u, weight) in row.iter_mut().enumerate() {
            let scale = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            *weight = scale * (((2 * x + 1) * u) as f32 * std::f32::consts::PI / 16.0).cos() / 2.0;
        }
    }
    let stride = component.blocks_wide * 8;
    let mut plane = vec![0u8; stride * component.blocks_high * 8];
    let mut input = [0f32; 64];
    let mut rows = [0f32; 64];
    let mut out = [0f32; 64];
    for (block, coefs) in component.coefs.as_chunks::<64>().0.iter().enumerate() {
        let (bx, by) = (block % component.blocks_wide * 8, block / component.blocks_wide * 8);
        if coefs[1..].iter().all(|&c| c == 0) {
            // Only the average: the same value everywhere.
            out.fill(coefs[0] as f32 * quant[0] as f32 / 8.0);
        } else {
            for i in 0..64 {
                input[i] = coefs[i] as f32 * quant[i] as f32;
            }
            // Along each row, then along each column.
            for y in 0..8 {
                for x in 0..8 {
                    rows[y * 8 + x] = (0..8).map(|u| table[x][u] * input[y * 8 + u]).sum();
                }
            }
            for y in 0..8 {
                for x in 0..8 {
                    out[y * 8 + x] = (0..8).map(|v| table[y][v] * rows[v * 8 + x]).sum();
                }
            }
        }
        for y in 0..8 {
            for x in 0..8 {
                plane[(by + y) * stride + bx + x] = (out[y * 8 + x] + 128.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    plane
}

/// Upsample the planes to the image size and convert them to RGBA.
fn to_rgba(frame: &Frame, planes: &[Vec<u8>], adobe_transform: Option<u8>) -> Option<Image> {
    let (width, height) = (frame.width, frame.height);
    let mut rgba = vec![255u8; width * height * 4];
    // For each component: its row length and, per image column, its column.
    let columns: Vec<Vec<usize>> = frame.components.iter().map(|c| (0..width).map(|x| x * c.h / frame.hmax).collect()).collect();
    let mut samples = [0u8; 4];
    for y in 0..height {
        for x in 0..width {
            for (i, c) in frame.components.iter().enumerate() {
                let row = y * c.v / frame.vmax;
                samples[i] = planes[i][row * c.blocks_wide * 8 + columns[i][x]];
            }
            let rgb = match (frame.components.len(), adobe_transform) {
                (1, _) => [samples[0]; 3],
                (3, Some(0)) => [samples[0], samples[1], samples[2]],
                (3, _) => ycc_to_rgb(samples[0], samples[1], samples[2]),
                // Adobe writes CMYK inverted: 255 means no ink.
                (4, Some(2)) => {
                    let [r, g, b] = ycc_to_rgb(samples[0], samples[1], samples[2]);
                    [scale(255 - r, samples[3]), scale(255 - g, samples[3]), scale(255 - b, samples[3])]
                }
                (4, _) => [scale(samples[0], samples[3]), scale(samples[1], samples[3]), scale(samples[2], samples[3])],
                _ => return None,
            };
            rgba[(y * width + x) * 4..][..3].copy_from_slice(&rgb);
        }
    }
    Some(Image { width, height, rgba })
}

fn ycc_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (y, cb, cr) = (y as f32, cb as f32 - 128.0, cr as f32 - 128.0);
    let channel = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [channel(y + 1.402 * cr), channel(y - 0.344_136 * cb - 0.714_136 * cr), channel(y + 1.772 * cb)]
}

/// `a * b / 255`, rounded.
fn scale(a: u8, b: u8) -> u8 {
    ((a as u32 * b as u32 + 127) / 255) as u8
}

/// Turn the image upright as the EXIF orientation says (1 = already upright;
/// 2 to 8 are the mirrored and rotated cases of the TIFF specification).
fn orient(image: Image, orientation: u16) -> Image {
    if orientation <= 1 || orientation > 8 {
        return image;
    }
    let (w, h) = (image.width, image.height);
    let swap = orientation >= 5;
    let (out_w, out_h) = if swap { (h, w) } else { (w, h) };
    let mut rgba = vec![0u8; image.rgba.len()];
    for y in 0..out_h {
        for x in 0..out_w {
            // The source pixel shown at (x, y).
            let (sx, sy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            let (from, to) = ((sy * w + sx) * 4, (y * out_w + x) * 4);
            rgba[to..to + 4].copy_from_slice(&image.rgba[from..from + 4]);
        }
    }
    Image { width: out_w, height: out_h, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test pictures: 32x24, red top-left, blue top-right, green bottom half.
    const BASELINE: &[u8] = include_bytes!("testdata/baseline.jpg"); // 4:2:0, a restart marker after every MCU
    const PROGRESSIVE: &[u8] = include_bytes!("testdata/progressive.jpg"); // 4:2:2
    const GRAY: &[u8] = include_bytes!("testdata/gray.jpg");

    fn pixel(image: &Image, x: usize, y: usize) -> [u8; 3] {
        let at = (y * image.width + x) * 4;
        [image.rgba[at], image.rgba[at + 1], image.rgba[at + 2]]
    }

    fn near(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(&a, b)| a.abs_diff(b) < 24)
    }

    #[test]
    fn decodes_baseline_and_progressive_files() {
        for (name, data) in [("baseline", BASELINE), ("progressive", PROGRESSIVE)] {
            let image = decode(data).unwrap_or_else(|| panic!("{name} decodes"));
            assert_eq!((image.width, image.height), (32, 24), "{name}");
            assert!(near(pixel(&image, 4, 4), [255, 0, 0]), "{name}: red, got {:?}", pixel(&image, 4, 4));
            assert!(near(pixel(&image, 27, 4), [0, 0, 255]), "{name}: blue, got {:?}", pixel(&image, 27, 4));
            assert!(near(pixel(&image, 16, 20), [0, 255, 0]), "{name}: green, got {:?}", pixel(&image, 16, 20));
        }
        let gray = decode(GRAY).expect("gray decodes");
        let [r, g, b] = pixel(&gray, 16, 20);
        assert!(r == g && g == b && r > 100, "green turns a light gray: {r}");
    }

    #[test]
    fn exif_orientation_turns_the_picture() {
        // An Exif segment saying "rotate 90 degrees clockwise to show".
        let mut tiff = b"MM\0\x2a\0\0\0\x08\0\x01".to_vec();
        tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0]);
        let mut segment = b"Exif\0\0".to_vec();
        segment.extend_from_slice(&tiff);
        let mut data = vec![0xff, 0xd8, 0xff, 0xe1];
        data.extend_from_slice(&((segment.len() + 2) as u16).to_be_bytes());
        data.extend_from_slice(&segment);
        data.extend_from_slice(&BASELINE[2..]);
        let image = decode(&data).expect("decodes");
        assert_eq!((image.width, image.height), (24, 32));
        // The green bottom half is now on the left, red at the top right.
        assert!(near(pixel(&image, 4, 16), [0, 255, 0]), "{:?}", pixel(&image, 4, 16));
        assert!(near(pixel(&image, 19, 4), [255, 0, 0]), "{:?}", pixel(&image, 19, 4));
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        assert!(decode(b"not a jpeg").is_none());
        assert!(decode(&[0xff, 0xd8, 0xff, 0xd9]).is_none());
        // Every truncation and many corruptions: no panic, whatever the result.
        for cut in 0..BASELINE.len() {
            let _ = decode(&BASELINE[..cut]);
        }
        for at in (2..PROGRESSIVE.len()).step_by(3) {
            let mut broken = PROGRESSIVE.to_vec();
            broken[at] ^= 0x5a;
            let _ = decode(&broken);
        }
    }
}
