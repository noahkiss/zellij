//! Reading an image file into RGBA. The format is told by the file's first bytes, not by its name,
//! so a screenshot saved without an extension still opens.

use std::io::Cursor;

use super::render::Rgba;

/// The most pixels the viewer decodes, about 8K by 8K. A larger file is refused with a message
/// rather than allocated: it is almost always a mistake, and the pane is a preview.
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// The formats the viewer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    WebP,
}

/// The format `bytes` begin like, or `None`.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Format::WebP)
    } else {
        None
    }
}

fn checked(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("the image has no pixels".to_owned());
    }
    if width as u64 * height as u64 > MAX_PIXELS {
        return Err(format!(
            "the image is {}x{}, larger than this viewer opens",
            width, height
        ));
    }
    Ok(())
}

/// The image in `bytes`, as RGBA. `Err` says why not, in words fit to show in the pane.
pub fn decode(bytes: &[u8]) -> Result<Rgba, String> {
    match sniff(bytes) {
        Some(Format::Png) => png_rgba(bytes),
        Some(Format::Jpeg) => jpeg_rgba(bytes),
        Some(Format::Gif) => gif_rgba(bytes),
        Some(Format::WebP) => webp_rgba(bytes),
        None => Err("not a PNG, JPEG, GIF or WebP image".to_owned()),
    }
}

fn png_rgba(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("cannot read the PNG: {}", e))?;
    let (width, height) = (reader.info().width, reader.info().height);
    checked(width, height)?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let out = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("cannot read the PNG: {}", e))?;
    buf.truncate(out.buffer_size());
    let pixels = match out.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => expand(&buf, 3, |p| [p[0], p[1], p[2], 255]),
        png::ColorType::Grayscale => expand(&buf, 1, |p| [p[0], p[0], p[0], 255]),
        png::ColorType::GrayscaleAlpha => expand(&buf, 2, |p| [p[0], p[0], p[0], p[1]]),
        // `normalize_to_color8` expands a palette, so this is not reached
        png::ColorType::Indexed => return Err("cannot read an indexed PNG".to_owned()),
    };
    Ok(Rgba {
        width: out.width,
        height: out.height,
        pixels,
    })
}

fn expand(buf: &[u8], channels: usize, to_rgba: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(buf.len() / channels * 4);
    for pixel in buf.chunks_exact(channels) {
        pixels.extend_from_slice(&to_rgba(pixel));
    }
    pixels
}

fn jpeg_rgba(bytes: &[u8]) -> Result<Rgba, String> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|e| format!("cannot read the JPEG: {:?}", e))?;
    let info = decoder
        .info()
        .ok_or_else(|| "cannot read the JPEG's size".to_owned())?;
    let (width, height) = (info.width as u32, info.height as u32);
    checked(width, height)?;
    let pixels = decoder
        .decode()
        .map_err(|e| format!("cannot read the JPEG: {:?}", e))?;
    if pixels.len() != width as usize * height as usize * 4 {
        return Err("cannot read the JPEG: it decoded to the wrong size".to_owned());
    }
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

/// The first frame of a GIF, laid on a transparent canvas of the GIF's own size: a frame may
/// cover only part of it.
fn gif_rgba(bytes: &[u8]) -> Result<Rgba, String> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(bytes))
        .map_err(|e| format!("cannot read the GIF: {}", e))?;
    let (width, height) = (decoder.width() as u32, decoder.height() as u32);
    checked(width, height)?;
    let frame = decoder
        .read_next_frame()
        .map_err(|e| format!("cannot read the GIF: {}", e))?
        .ok_or_else(|| "the GIF has no frames".to_owned())?;
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let (left, top) = (frame.left as usize, frame.top as usize);
    let frame_width = frame.width as usize;
    for (row, line) in frame.buffer.chunks_exact(frame_width * 4).enumerate() {
        let y = top + row;
        if y >= height as usize {
            break;
        }
        let visible = frame_width.min((width as usize).saturating_sub(left));
        let at = (y * width as usize + left) * 4;
        pixels[at..at + visible * 4].copy_from_slice(&line[..visible * 4]);
    }
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

fn webp_rgba(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
        .map_err(|e| format!("cannot read the WebP: {}", e))?;
    let (width, height) = decoder.dimensions();
    checked(width, height)?;
    let size = decoder
        .output_buffer_size()
        .ok_or_else(|| "the WebP is too large".to_owned())?;
    let mut buf = vec![0u8; size];
    decoder
        .read_image(&mut buf)
        .map_err(|e| format!("cannot read the WebP: {}", e))?;
    let pixels = if decoder.has_alpha() {
        buf
    } else {
        expand(&buf, 3, |p| [p[0], p[1], p[2], 255])
    };
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(data).expect("data");
        }
        out
    }

    #[test]
    fn the_format_is_told_by_its_first_bytes() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some(Format::Png));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some(Format::Jpeg));
        assert_eq!(sniff(b"GIF89a...."), Some(Format::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::WebP));
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn a_png_of_each_colour_type_comes_back_as_rgba() {
        let rgb = png_bytes(2, 1, png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(
            decode(&rgb).expect("png"),
            Rgba {
                width: 2,
                height: 1,
                pixels: vec![1, 2, 3, 255, 4, 5, 6, 255],
            }
        );
        let grey = png_bytes(1, 1, png::ColorType::GrayscaleAlpha, &[9, 128]);
        assert_eq!(decode(&grey).expect("png").pixels, vec![9, 9, 9, 128]);
        let rgba = png_bytes(1, 1, png::ColorType::Rgba, &[1, 2, 3, 4]);
        assert_eq!(decode(&rgba).expect("png").pixels, vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_gif_frame_is_laid_on_its_canvas() {
        let mut out = Vec::new();
        {
            let palette = [255, 0, 0, 0, 0, 255];
            let mut encoder = gif::Encoder::new(&mut out, 2, 2, &palette).expect("encoder");
            let mut frame = gif::Frame::default();
            frame.width = 1;
            frame.height = 1;
            frame.left = 1;
            frame.top = 1;
            frame.buffer = std::borrow::Cow::Borrowed(&[1]);
            encoder.write_frame(&frame).expect("frame");
        }
        let image = decode(&out).expect("gif");
        assert_eq!((image.width, image.height), (2, 2));
        // only the bottom-right pixel is drawn, in the second palette colour
        assert_eq!(&image.pixels[12..16], &[0, 0, 255, 255]);
        assert_eq!(&image.pixels[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn what_cannot_be_read_says_why() {
        assert_eq!(
            decode(b"plain text").unwrap_err(),
            "not a PNG, JPEG, GIF or WebP image"
        );
        assert!(decode(b"\x89PNG\r\n\x1a\ntruncated")
            .unwrap_err()
            .starts_with("cannot read the PNG"));
        assert!(checked(10_000, 10_000).unwrap_err().contains("10000x10000"));
        assert!(checked(0, 5).is_err());
    }
}
