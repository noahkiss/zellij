//! The half of the viewer that does not touch a terminal: where an image goes, how big, what the
//! escape sequences are, and what a key means. Everything here is a function of its inputs, which
//! is what lets the tests at the bottom pin it down without a pty.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use zellij_utils::cli::ImageMode;

/// A decoded image: 8-bit RGBA, row-major, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The size of one terminal cell, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

/// What a cell is assumed to measure when the pty does not say. Only the ratio matters to the
/// half-block renderer, and kitty is told how many cells to fill, so a wrong guess distorts the
/// aspect a little rather than drawing in the wrong place.
pub const ASSUMED_CELL: CellSize = CellSize {
    width: 8,
    height: 16,
};

impl CellSize {
    /// The cell size a window of `cols` by `rows` cells and `px_width` by `px_height` pixels has,
    /// or [`ASSUMED_CELL`] when the pixel size is unknown (zero) or does not divide sensibly.
    pub fn from_window(cols: u16, rows: u16, px_width: u16, px_height: u16) -> CellSize {
        if cols == 0 || rows == 0 || px_width == 0 || px_height == 0 {
            return ASSUMED_CELL;
        }
        let width = px_width as u32 / cols as u32;
        let height = px_height as u32 / rows as u32;
        if width == 0 || height == 0 {
            return ASSUMED_CELL;
        }
        CellSize { width, height }
    }
}

/// Where an image goes inside the image area and how big it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    /// Offset from the area's left edge, in cells.
    pub col: u16,
    /// Offset from the area's top edge, in cells.
    pub row: u16,
    /// Width of the box the image fills, in cells.
    pub cols: u16,
    /// Height of the box the image fills, in cells.
    pub rows: u16,
    /// The pixel size to resample the image to before it is sent.
    pub px_width: u32,
    pub px_height: u32,
}

fn centred(area: u16, used: u16) -> u16 {
    area.saturating_sub(used) / 2
}

/// The box a kitty image fills: as large as the area allows at the image's own aspect, and never
/// larger than the image itself, so a small screenshot is shown at its real size rather than
/// blown up. `None` when the area has no room at all.
pub fn fit_kitty(
    width: u32,
    height: u32,
    area_cols: u16,
    area_rows: u16,
    cell: CellSize,
) -> Option<Fit> {
    if width == 0 || height == 0 || area_cols == 0 || area_rows == 0 {
        return None;
    }
    let area_px_w = area_cols as f64 * cell.width as f64;
    let area_px_h = area_rows as f64 * cell.height as f64;
    let scale = (area_px_w / width as f64)
        .min(area_px_h / height as f64)
        .min(1.0);
    let px_width = ((width as f64 * scale).round() as u32).max(1);
    let px_height = ((height as f64 * scale).round() as u32).max(1);
    let cols = (px_width.div_ceil(cell.width) as u16).clamp(1, area_cols);
    let rows = (px_height.div_ceil(cell.height) as u16).clamp(1, area_rows);
    Some(Fit {
        col: centred(area_cols, cols),
        row: centred(area_rows, rows),
        cols,
        rows,
        px_width,
        px_height,
    })
}

/// The box a half-block image fills. Each cell carries two pixels stacked, so the image is
/// resampled to `cols` by `2 * rows` pixels, sized so that the drawn pixels - a cell wide and half
/// a cell high - keep the image's aspect. Small images are scaled up: a 16-pixel icon drawn as 16
/// half-blocks would be unreadable.
pub fn fit_blocks(
    width: u32,
    height: u32,
    area_cols: u16,
    area_rows: u16,
    cell: CellSize,
) -> Option<Fit> {
    if width == 0 || height == 0 || area_cols == 0 || area_rows == 0 {
        return None;
    }
    // in physical pixels, then back into block pixels: cell.width wide, cell.height / 2 high
    let scale = (area_cols as f64 * cell.width as f64 / width as f64)
        .min(area_rows as f64 * cell.height as f64 / height as f64);
    let block_w = cell.width as f64;
    let block_h = cell.height as f64 / 2.0;
    let px_width = ((width as f64 * scale / block_w).round() as u32).clamp(1, area_cols as u32);
    let px_height =
        ((height as f64 * scale / block_h).round() as u32).clamp(1, area_rows as u32 * 2);
    let cols = px_width as u16;
    let rows = px_height.div_ceil(2) as u16;
    Some(Fit {
        col: centred(area_cols, cols),
        row: centred(area_rows, rows),
        cols,
        rows,
        px_width,
        px_height,
    })
}

/// The image resampled to `width` by `height`: each target pixel is the average of the source
/// pixels under it when shrinking, and the nearest source pixel when growing.
pub fn resample(image: &Rgba, width: u32, height: u32) -> Rgba {
    if image.width == width && image.height == height {
        return image.clone();
    }
    let (sw, sh) = (image.width as u64, image.height as u64);
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as u64 {
        let y0 = y * sh / height as u64;
        let y1 = ((y + 1) * sh / height as u64).max(y0 + 1).min(sh);
        for x in 0..width as u64 {
            let x0 = x * sw / width as u64;
            let x1 = ((x + 1) * sw / width as u64).max(x0 + 1).min(sw);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                let row = (sy * sw) as usize * 4;
                for sx in x0..x1 {
                    let at = row + sx as usize * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += image.pixels[at + channel] as u64;
                    }
                }
            }
            let count = (y1 - y0) * (x1 - x0);
            for total in sum {
                pixels.push((total / count) as u8);
            }
        }
    }
    Rgba {
        width,
        height,
        pixels,
    }
}

/// The largest base64 payload one kitty escape carries, as the protocol asks of a client.
pub const KITTY_CHUNK: usize = 4096;

/// The escapes that transmit `image` and place it at the cursor, filling `cols` by `rows` cells.
///
/// RGBA (`f=32`), zlib-compressed (`o=z`), sent in the stream itself (`t=d`) so it travels over
/// ssh, and split into chunks of at most [`KITTY_CHUNK`] base64 bytes (`m=1` on every chunk but
/// the last). `C=1` leaves the cursor where it was and `q=2` asks for no reply, which would
/// otherwise arrive on stdin among the keys.
pub fn kitty_transmit(id: u32, image: &Rgba, cols: u16, rows: u16) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    // writing into a Vec cannot fail
    let _ = encoder.write_all(&image.pixels);
    let compressed = encoder.finish().unwrap_or_default();
    let payload = BASE64.encode(compressed);
    let chunks: Vec<&[u8]> = if payload.is_empty() {
        vec![&[][..]]
    } else {
        payload.as_bytes().chunks(KITTY_CHUNK).collect()
    };
    let last = chunks.len() - 1;
    let mut out = Vec::with_capacity(payload.len() + chunks.len() * 32 + 96);
    for (index, chunk) in chunks.iter().enumerate() {
        let more = if index == last { 0 } else { 1 };
        if index == 0 {
            let _ = write!(
                out,
                "\x1b_Ga=T,f=32,o=z,t=d,i={},s={},v={},c={},r={},C=1,q=2,m={};",
                id, image.width, image.height, cols, rows, more
            );
        } else {
            let _ = write!(out, "\x1b_Gq=2,m={};", more);
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// The escape that deletes image `id` and frees its data, so nothing of it is left in the
/// terminal - the uppercase `I` is what frees the data rather than only hiding the placement.
pub fn kitty_delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", id).into_bytes()
}

/// The question that asks whether the terminal draws kitty graphics, followed by a primary device
/// attributes request every terminal answers. A terminal that understands the first answers it
/// before the second; one that does not answers only the second, so the wait is bounded by the
/// terminal's own reply rather than by a timeout.
pub fn kitty_probe(id: u32) -> Vec<u8> {
    format!("\x1b_Gi={},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c", id).into_bytes()
}

/// What the replies to [`kitty_probe`] say so far: `Some(true)` for an `OK`, `Some(false)` for an
/// error or a device-attributes answer with no `OK` before it, and `None` while neither has
/// arrived. Inside zellij the answer is the session's: it says `OK` only when a terminal attached
/// to it can draw kitty graphics.
pub fn probe_answer(replies: &[u8]) -> Option<bool> {
    let mut at = 0;
    while at < replies.len() {
        let rest = &replies[at..];
        if rest.starts_with(b"\x1b_G") {
            let end = find(rest, b"\x1b\\")?;
            let body = &rest[3..end];
            let status = body
                .iter()
                .position(|b| *b == b';')
                .map(|semi| &body[semi + 1..])
                .unwrap_or(&[]);
            return Some(status == b"OK");
        }
        if rest.starts_with(b"\x1b[?") {
            // a device-attributes answer ends in `c`; anything else is some other report
            let end = rest[3..]
                .iter()
                .position(|b| (0x40..=0x7e).contains(b))
                .map(|pos| pos + 3)?;
            if rest[end] == b'c' {
                return Some(false);
            }
            at += end + 1;
            continue;
        }
        at += 1;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// How the viewer draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    Kitty,
    Blocks,
}

/// The renderer for a requested mode, given what the probe found. The probe is only asked for in
/// `auto`; an unanswered one (`None`) falls back to half-blocks, which every terminal draws.
pub fn choose_renderer(requested: ImageMode, probe: Option<bool>) -> Renderer {
    match requested {
        ImageMode::Kitty => Renderer::Kitty,
        ImageMode::Blocks => Renderer::Blocks,
        ImageMode::Auto => {
            if probe == Some(true) {
                Renderer::Kitty
            } else {
                Renderer::Blocks
            }
        },
    }
}

/// The image drawn as rows of `▀`, the upper pixel in the foreground colour and the lower in the
/// background. Transparent pixels are laid over black. Each row starts at `(top + n, left)`,
/// 1-based, and resets its colours at the end so nothing bleeds into the margin.
pub fn blocks(image: &Rgba, top: u16, left: u16) -> String {
    let pixel = |x: u32, y: u32| -> [u8; 3] {
        if y >= image.height {
            return [0, 0, 0];
        }
        let at = ((y * image.width + x) * 4) as usize;
        let alpha = image.pixels[at + 3] as u32;
        [0, 1, 2].map(|c| (image.pixels[at + c] as u32 * alpha / 255) as u8)
    };
    let mut out = String::new();
    for row in 0..image.height.div_ceil(2) {
        let _ = write!(out, "\x1b[{};{}H", top as u32 + row, left);
        let mut last: Option<([u8; 3], [u8; 3])> = None;
        for x in 0..image.width {
            let upper = pixel(x, row * 2);
            let lower = pixel(x, row * 2 + 1);
            if last != Some((upper, lower)) {
                let _ = write!(
                    out,
                    "\x1b[38;2;{};{};{};48;2;{};{};{}m",
                    upper[0], upper[1], upper[2], lower[0], lower[1], lower[2]
                );
                last = Some((upper, lower));
            }
            out.push('▀');
        }
        out.push_str("\x1b[0m");
    }
    out
}

/// Which image is showing, out of how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pager {
    pub index: usize,
    pub len: usize,
}

impl Pager {
    pub fn new(len: usize) -> Self {
        Pager { index: 0, len }
    }
    /// The next image, wrapping from the last to the first.
    pub fn next(&mut self) {
        if self.len > 0 {
            self.index = (self.index + 1) % self.len;
        }
    }
    /// The previous image, wrapping from the first to the last.
    pub fn previous(&mut self) {
        if self.len > 0 {
            self.index = (self.index + self.len - 1) % self.len;
        }
    }
    pub fn first(&mut self) {
        self.index = 0;
    }
    pub fn last(&mut self) {
        self.index = self.len.saturating_sub(1);
    }
}

/// The title of the image showing: its place and its file name, as in `2/5 screenshot.png`.
pub fn title(pager: &Pager, path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    format!("{}/{} {}", pager.index + 1, pager.len, name)
}

/// The key line at the foot of the viewer.
pub fn footer(len: usize) -> &'static str {
    if len > 1 {
        "←/→ flip   q close"
    } else {
        "q close"
    }
}

/// `text` cut to `width` characters, with an ellipsis where it was cut.
pub fn clip(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut clipped: String = text.chars().take(width - 1).collect();
    clipped.push('…');
    clipped
}

/// What a key asks the viewer to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Next,
    Previous,
    First,
    Last,
    Redraw,
    Quit,
}

/// The keys in one read from the terminal. Arrows come in either cursor-key mode; an escape with
/// nothing after it in the same read is the Esc key. Sequences that mean nothing here - another
/// CSI, a kitty graphics reply - are skipped whole, so their bytes are never read as keys.
pub fn keys(input: &[u8]) -> Vec<Key> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < input.len() {
        let byte = input[at];
        if byte == 0x1b {
            let rest = &input[at + 1..];
            match rest.first() {
                None => {
                    found.push(Key::Quit);
                    at += 1;
                },
                Some(b'[') | Some(b'O') => {
                    // parameters and intermediates, then one final byte
                    let body = &rest[1..];
                    match body.iter().position(|b| (0x40..=0x7e).contains(b)) {
                        Some(end) => {
                            let params = &body[..end];
                            let key = match (rest[0], body[end], params) {
                                (_, b'C', _) | (_, b'B', _) => Some(Key::Next),
                                (_, b'D', _) | (_, b'A', _) => Some(Key::Previous),
                                (_, b'H', _) | (b'[', b'~', b"1") | (b'[', b'~', b"7") => {
                                    Some(Key::First)
                                },
                                (_, b'F', _) | (b'[', b'~', b"4") | (b'[', b'~', b"8") => {
                                    Some(Key::Last)
                                },
                                (b'[', b'~', b"5") => Some(Key::Previous),
                                (b'[', b'~', b"6") => Some(Key::Next),
                                // Esc under the kitty keyboard protocol
                                (b'[', b'u', b"27") => Some(Key::Quit),
                                _ => None,
                            };
                            found.extend(key);
                            at += 2 + end + 1;
                        },
                        None => at = input.len(),
                    }
                },
                Some(b'_') | Some(b']') | Some(b'P') => {
                    // a string sequence: skip to its terminator
                    match find(rest, b"\x1b\\")
                        .map(|end| end + 2)
                        .or_else(|| rest.iter().position(|b| *b == 0x07).map(|end| end + 1))
                    {
                        Some(end) => at += 1 + end,
                        None => at = input.len(),
                    }
                },
                // Alt plus a key: not one of ours
                Some(_) => at += 2,
            }
            continue;
        }
        let key = match byte {
            b'q' | b'Q' | 0x03 => Some(Key::Quit),
            b'l' | b'n' | b'j' | b' ' => Some(Key::Next),
            b'h' | b'p' | b'k' | b'b' | 0x7f | 0x08 => Some(Key::Previous),
            b'g' => Some(Key::First),
            b'G' => Some(Key::Last),
            b'r' | 0x0c => Some(Key::Redraw),
            _ => None,
        };
        found.extend(key);
        at += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Rgba {
        Rgba {
            width,
            height,
            pixels: rgba.repeat((width * height) as usize),
        }
    }

    #[test]
    fn the_cell_size_comes_from_the_window_or_is_assumed() {
        assert_eq!(
            CellSize::from_window(80, 24, 800, 480),
            CellSize {
                width: 10,
                height: 20
            }
        );
        assert_eq!(CellSize::from_window(80, 24, 0, 0), ASSUMED_CELL);
        assert_eq!(CellSize::from_window(0, 24, 800, 480), ASSUMED_CELL);
        assert_eq!(CellSize::from_window(80, 24, 40, 480), ASSUMED_CELL);
    }

    #[test]
    fn a_large_image_is_scaled_down_to_the_area_at_its_own_aspect() {
        // 80x20 cells is 800x400 pixels; a 1600x400 image is width-bound at half size
        let fit = fit_kitty(1600, 400, 80, 20, CELL).expect("room");
        assert_eq!((fit.px_width, fit.px_height), (800, 200));
        assert_eq!((fit.cols, fit.rows), (80, 10));
        // centred vertically in the five rows either side
        assert_eq!((fit.col, fit.row), (0, 5));
    }

    #[test]
    fn a_small_image_is_shown_at_its_own_size_and_centred() {
        let fit = fit_kitty(100, 40, 80, 20, CELL).expect("room");
        assert_eq!((fit.px_width, fit.px_height), (100, 40));
        assert_eq!((fit.cols, fit.rows), (10, 2));
        assert_eq!((fit.col, fit.row), (35, 9));
    }

    #[test]
    fn a_tall_image_is_height_bound() {
        let fit = fit_kitty(400, 4000, 80, 20, CELL).expect("room");
        assert_eq!((fit.px_width, fit.px_height), (40, 400));
        assert_eq!((fit.cols, fit.rows), (4, 20));
        assert_eq!(fit.row, 0);
    }

    #[test]
    fn nothing_fits_into_no_room() {
        assert_eq!(fit_kitty(100, 100, 0, 20, CELL), None);
        assert_eq!(fit_kitty(100, 100, 80, 0, CELL), None);
        assert_eq!(fit_blocks(0, 100, 80, 20, CELL), None);
    }

    #[test]
    fn half_blocks_keep_the_aspect_with_two_pixels_a_cell() {
        // a square image in a 10x20 cell: 20 rows are 400 physical pixels high, so the square is
        // 400 wide - 40 cells - and 40 block pixels high, two to a cell
        let fit = fit_blocks(100, 100, 80, 20, CELL).expect("room");
        assert_eq!((fit.px_width, fit.px_height), (40, 40));
        assert_eq!((fit.cols, fit.rows), (40, 20));
        assert_eq!((fit.col, fit.row), (20, 0));
        // and a small icon is scaled up rather than drawn at sixteen blocks
        let icon = fit_blocks(16, 16, 80, 20, CELL).expect("room");
        assert_eq!((icon.px_width, icon.px_height), (40, 40));
    }

    #[test]
    fn resampling_averages_when_shrinking_and_repeats_when_growing() {
        let image = Rgba {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 255, 200, 100, 50, 255],
        };
        let shrunk = resample(&image, 1, 1);
        assert_eq!(shrunk.pixels, vec![100, 50, 25, 255]);
        let grown = resample(&image, 4, 1);
        assert_eq!(
            grown.pixels,
            vec![0, 0, 0, 255, 0, 0, 0, 255, 200, 100, 50, 255, 200, 100, 50, 255]
        );
        let same = resample(&image, 2, 1);
        assert_eq!(same, image);
    }

    /// The control keys and the payload of every escape in a transmit.
    fn kitty_escapes(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut escapes = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            assert!(rest.starts_with(b"\x1b_G"), "every escape is an APC G");
            let end = find(rest, b"\x1b\\").expect("terminated");
            let body = &rest[3..end];
            let semi = body.iter().position(|b| *b == b';').expect("a payload");
            escapes.push((
                String::from_utf8(body[..semi].to_vec()).unwrap(),
                body[semi + 1..].to_vec(),
            ));
            rest = &rest[end + 2..];
        }
        escapes
    }

    #[test]
    fn a_kitty_transmit_is_chunked_and_carries_the_image_it_was_given() {
        // noise compresses badly, so the payload needs several chunks
        let mut pixels = Vec::new();
        let mut seed: u32 = 7;
        for _ in 0..(64 * 64 * 4) {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            pixels.push((seed >> 16) as u8);
        }
        let image = Rgba {
            width: 64,
            height: 64,
            pixels,
        };
        let escapes = kitty_escapes(&kitty_transmit(42, &image, 7, 3));
        assert!(escapes.len() > 1, "more than one chunk");
        assert_eq!(
            escapes[0].0,
            "a=T,f=32,o=z,t=d,i=42,s=64,v=64,c=7,r=3,C=1,q=2,m=1"
        );
        for (index, (control, payload)) in escapes.iter().enumerate() {
            assert!(payload.len() <= KITTY_CHUNK);
            let last = index == escapes.len() - 1;
            if index > 0 {
                assert_eq!(control, if last { "q=2,m=0" } else { "q=2,m=1" });
            }
            if !last {
                // every chunk but the last is a whole number of base64 quads
                assert_eq!(payload.len() % 4, 0);
            }
        }
        let joined: Vec<u8> = escapes.iter().flat_map(|(_, p)| p.clone()).collect();
        let compressed = BASE64.decode(joined).expect("base64");
        let mut inflated = Vec::new();
        ZlibDecoder::new(&compressed[..])
            .read_to_end(&mut inflated)
            .expect("zlib");
        assert_eq!(inflated, image.pixels);
    }

    #[test]
    fn a_small_transmit_is_one_escape() {
        let escapes = kitty_escapes(&kitty_transmit(1, &solid(2, 2, [1, 2, 3, 255]), 1, 1));
        assert_eq!(escapes.len(), 1);
        assert!(escapes[0].0.ends_with(",m=0"));
    }

    #[test]
    fn the_delete_frees_the_image_and_asks_for_no_reply() {
        assert_eq!(kitty_delete(9), b"\x1b_Ga=d,d=I,i=9,q=2\x1b\\".to_vec());
    }

    #[test]
    fn the_probe_is_answered_by_an_ok_or_by_the_device_attributes_alone() {
        assert!(kitty_probe(5).ends_with(b"\x1b[c"));
        assert_eq!(probe_answer(b""), None);
        assert_eq!(probe_answer(b"\x1b_Gi=5;OK\x1b\\\x1b[?62;4c"), Some(true));
        assert_eq!(probe_answer(b"\x1b_Gi=5;OK\x1b\\"), Some(true));
        // zellij's answer when no attached terminal can draw kitty graphics
        assert_eq!(
            probe_answer(b"\x1b_Gi=5;ENOTSUPPORTED:kitty graphics not supported\x1b\\"),
            Some(false)
        );
        // a terminal that does not speak the protocol answers only the second question
        assert_eq!(probe_answer(b"\x1b[?62;22c"), Some(false));
        // a half-read reply is not an answer yet, and a stray key before it is passed over
        assert_eq!(probe_answer(b"\x1b_Gi=5;O"), None);
        assert_eq!(probe_answer(b"x\x1b[?6c"), Some(false));
    }

    #[test]
    fn auto_draws_kitty_only_when_the_terminal_said_yes() {
        assert_eq!(
            choose_renderer(ImageMode::Auto, Some(true)),
            Renderer::Kitty
        );
        assert_eq!(
            choose_renderer(ImageMode::Auto, Some(false)),
            Renderer::Blocks
        );
        assert_eq!(choose_renderer(ImageMode::Auto, None), Renderer::Blocks);
        assert_eq!(
            choose_renderer(ImageMode::Kitty, Some(false)),
            Renderer::Kitty
        );
        assert_eq!(
            choose_renderer(ImageMode::Blocks, Some(true)),
            Renderer::Blocks
        );
    }

    #[test]
    fn half_blocks_put_two_pixels_in_a_cell_and_reset_at_the_end_of_a_row() {
        let image = Rgba {
            width: 1,
            height: 2,
            pixels: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        assert_eq!(
            blocks(&image, 3, 5),
            "\x1b[3;5H\x1b[38;2;255;0;0;48;2;0;0;255m▀\x1b[0m"
        );
        // an odd height leaves the lower half of the last row black, and transparency is black
        let odd = solid(2, 1, [10, 20, 30, 0]);
        assert_eq!(
            blocks(&odd, 1, 1),
            "\x1b[1;1H\x1b[38;2;0;0;0;48;2;0;0;0m▀▀\x1b[0m"
        );
    }

    #[test]
    fn paging_wraps_at_both_ends() {
        let mut pager = Pager::new(3);
        pager.previous();
        assert_eq!(pager.index, 2);
        pager.next();
        assert_eq!(pager.index, 0);
        pager.next();
        pager.next();
        assert_eq!(pager.index, 2);
        pager.first();
        assert_eq!(pager.index, 0);
        pager.last();
        assert_eq!(pager.index, 2);
        let mut one = Pager::new(1);
        one.next();
        one.previous();
        assert_eq!(one.index, 0);
    }

    #[test]
    fn the_title_is_the_place_and_the_file_name() {
        let mut pager = Pager::new(5);
        pager.next();
        assert_eq!(
            title(&pager, Path::new("/tmp/shots/screenshot.png")),
            "2/5 screenshot.png"
        );
        assert_eq!(footer(5), "←/→ flip   q close");
        assert_eq!(footer(1), "q close");
        assert_eq!(clip("screenshot.png", 8), "screens…");
        assert_eq!(clip("a.png", 8), "a.png");
        assert_eq!(clip("a.png", 0), "");
    }

    #[test]
    fn keys_are_read_in_either_cursor_mode_and_esc_alone_quits() {
        assert_eq!(
            keys(b"\x1b[C\x1bOC\x1b[D\x1bOD"),
            vec![Key::Next, Key::Next, Key::Previous, Key::Previous]
        );
        assert_eq!(keys(b"\x1b"), vec![Key::Quit]);
        assert_eq!(keys(b"q"), vec![Key::Quit]);
        assert_eq!(keys(b"\x03"), vec![Key::Quit]);
        assert_eq!(keys(b"\x1b[27u"), vec![Key::Quit]);
        assert_eq!(keys(b"\x1b[5~\x1b[6~"), vec![Key::Previous, Key::Next]);
        assert_eq!(
            keys(b"\x1b[H\x1b[F\x1b[1~\x1b[4~"),
            vec![Key::First, Key::Last, Key::First, Key::Last]
        );
        assert_eq!(
            keys(b" lhgG"),
            vec![Key::Next, Key::Next, Key::Previous, Key::First, Key::Last]
        );
    }

    #[test]
    fn other_sequences_are_skipped_whole_rather_than_read_as_keys() {
        // a kitty reply carries a `q`-free payload but an `i=` and an `OK` that would read as keys
        assert_eq!(keys(b"\x1b_Gi=1;OK\x1b\\l"), vec![Key::Next]);
        // a device-attributes answer, a focus report and an Alt chord
        assert_eq!(keys(b"\x1b[?62;4c\x1b[I\x1bq"), Vec::<Key>::new());
        // an OSC answer ended by BEL
        assert_eq!(keys(b"\x1b]11;rgb:0/0/0\x07h"), vec![Key::Previous]);
    }
}
