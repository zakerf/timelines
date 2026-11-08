//! Cover-image validation: magic-byte sniffing and a header-only dimension
//! parse (PRD §9).
//!
//! Nothing here decodes or re-encodes the bytes. The stored object is exactly
//! what the owner uploaded; this module only answers "is it one of the three
//! accepted formats, and what size is it?".

pub const MAX_UPLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Immutable media objects: a cover change is a new key, never an overwrite.
pub const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    WebP,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::WebP => "webp",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidImage {
    TooLarge { bytes: usize },
    Gif,
    Svg,
    Unsupported,
    Truncated,
    BadDimensions,
}

impl InvalidImage {
    pub fn message(&self) -> String {
        match self {
            Self::TooLarge { bytes } => {
                format!("cover is {bytes} bytes; the limit is {MAX_UPLOAD_BYTES} bytes")
            }
            Self::Gif => "GIF covers are not accepted; use JPEG, PNG, or WebP".to_owned(),
            Self::Svg => "SVG covers are not accepted; use JPEG, PNG, or WebP".to_owned(),
            Self::Unsupported => "cover must be JPEG, PNG, or WebP".to_owned(),
            Self::Truncated => "cover bytes are truncated or malformed".to_owned(),
            Self::BadDimensions => "cover has no readable width/height".to_owned(),
        }
    }
}

/// Validate the bytes and read the intrinsic dimensions.
pub fn inspect(bytes: &[u8]) -> Result<Image, InvalidImage> {
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(InvalidImage::TooLarge { bytes: bytes.len() });
    }

    let format = sniff(bytes)?;
    let (width, height) = dimensions(bytes, format)?;
    Ok(Image {
        format,
        width,
        height,
    })
}

/// Magic bytes only: extension and declared `Content-Type` are never trusted.
pub fn sniff(bytes: &[u8]) -> Result<ImageFormat, InvalidImage> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG: &[u8] = b"\xff\xd8\xff";

    if bytes.starts_with(PNG) {
        return Ok(ImageFormat::Png);
    }
    if bytes.starts_with(JPEG) {
        return Ok(ImageFormat::Jpeg);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok(ImageFormat::WebP);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Err(InvalidImage::Gif);
    }
    if looks_like_svg(bytes) {
        return Err(InvalidImage::Svg);
    }
    Err(InvalidImage::Unsupported)
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(1024)];
    let Ok(text) = std::str::from_utf8(window) else {
        return false;
    };
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<svg") || text.starts_with("<?xml") && text.contains("<svg")
}

/// Server-chosen immutable key: `cover/<timelineId>/<uuid>.<ext>`.
pub fn cover_key(timeline_id: &str, object_id: &str, format: ImageFormat) -> String {
    format!("cover/{timeline_id}/{object_id}.{}", format.extension())
}

/// A v4 UUID, so a replacement never collides with (or overwrites) the old key.
pub fn new_object_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn dimensions(bytes: &[u8], format: ImageFormat) -> Result<(u32, u32), InvalidImage> {
    match format {
        ImageFormat::Png => png_dimensions(bytes),
        ImageFormat::Jpeg => jpeg_dimensions(bytes),
        ImageFormat::WebP => webp_dimensions(bytes),
    }
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), InvalidImage> {
    // Signature (8) + IHDR length (4) + "IHDR" (4) + width (4) + height (4).
    if bytes.len() < 24 {
        return Err(InvalidImage::Truncated);
    }
    if &bytes[12..16] != b"IHDR" {
        return Err(InvalidImage::BadDimensions);
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    positive(width, height)
}

fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32), InvalidImage> {
    let mut offset = 2;
    while offset + 1 < bytes.len() {
        if bytes[offset] != 0xFF {
            return Err(InvalidImage::Truncated);
        }
        // Fill bytes are allowed between segments.
        let mut marker = bytes[offset + 1];
        while marker == 0xFF {
            offset += 1;
            if offset + 1 >= bytes.len() {
                return Err(InvalidImage::Truncated);
            }
            marker = bytes[offset + 1];
        }
        offset += 2;

        match marker {
            0x01 | 0xD0..=0xD7 => continue,
            0xD9 => break,
            _ => {}
        }

        if offset + 2 > bytes.len() {
            return Err(InvalidImage::Truncated);
        }
        let length = usize::from(u16::from_be_bytes([bytes[offset], bytes[offset + 1]]));
        if length < 2 || offset + length > bytes.len() {
            return Err(InvalidImage::Truncated);
        }

        if is_start_of_frame(marker) {
            // length (2) + precision (1) + height (2) + width (2).
            if length < 7 {
                return Err(InvalidImage::BadDimensions);
            }
            let height = u32::from(u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]));
            let width = u32::from(u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]));
            return positive(width, height);
        }

        offset += length;
    }

    Err(InvalidImage::BadDimensions)
}

fn is_start_of_frame(marker: u8) -> bool {
    matches!(
        marker,
        0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
    )
}

fn webp_dimensions(bytes: &[u8]) -> Result<(u32, u32), InvalidImage> {
    if bytes.len() < 16 {
        return Err(InvalidImage::Truncated);
    }
    match &bytes[12..16] {
        b"VP8 " => {
            if bytes.len() < 30 {
                return Err(InvalidImage::Truncated);
            }
            let width = u32::from(u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF);
            let height = u32::from(u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF);
            positive(width, height)
        }
        b"VP8L" => {
            if bytes.len() < 25 || bytes[20] != 0x2F {
                return Err(InvalidImage::BadDimensions);
            }
            let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            let width = (bits & 0x3FFF) + 1;
            let height = ((bits >> 14) & 0x3FFF) + 1;
            positive(width, height)
        }
        b"VP8X" => {
            if bytes.len() < 30 {
                return Err(InvalidImage::Truncated);
            }
            let width = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]) + 1;
            let height = u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]) + 1;
            positive(width, height)
        }
        _ => Err(InvalidImage::BadDimensions),
    }
}

fn positive(width: u32, height: u32) -> Result<(u32, u32), InvalidImage> {
    if width == 0 || height == 0 {
        return Err(InvalidImage::BadDimensions);
    }
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes
    }

    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8];
        // APP0/JFIF, exactly like an encoded file.
        bytes.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        bytes.extend_from_slice(b"JFIF\0");
        bytes.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
        // SOF0.
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        bytes
    }

    fn webp_vp8(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&22u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8 ");
        bytes.extend_from_slice(&10u32.to_le_bytes());
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x9D, 0x01, 0x2A]);
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes
    }

    fn webp_lossless(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&17u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8L");
        bytes.extend_from_slice(&5u32.to_le_bytes());
        bytes.push(0x2F);
        let bits = (width - 1) | ((height - 1) << 14);
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes
    }

    #[test]
    fn accepts_png_and_reads_its_dimensions() {
        let image = inspect(&png(1600, 900)).expect("PNG is accepted");
        assert_eq!(image.format, ImageFormat::Png);
        assert_eq!((image.width, image.height), (1600, 900));
    }

    #[test]
    fn accepts_jpeg_after_skipping_metadata_segments() {
        let image = inspect(&jpeg(1600, 900)).expect("JPEG is accepted");
        assert_eq!(image.format, ImageFormat::Jpeg);
        assert_eq!((image.width, image.height), (1600, 900));
    }

    #[test]
    fn accepts_lossy_and_lossless_webp() {
        let lossy = inspect(&webp_vp8(1600, 900)).expect("WebP VP8 is accepted");
        assert_eq!(lossy.format, ImageFormat::WebP);
        assert_eq!((lossy.width, lossy.height), (1600, 900));

        let lossless = inspect(&webp_lossless(1200, 630)).expect("WebP VP8L is accepted");
        assert_eq!((lossless.width, lossless.height), (1200, 630));
    }

    #[test]
    fn rejects_gif_even_though_it_has_a_magic_number() {
        assert_eq!(inspect(b"GIF89a\x01\x00\x01\x00"), Err(InvalidImage::Gif));
        assert_eq!(inspect(b"GIF87a\x01\x00\x01\x00"), Err(InvalidImage::Gif));
    }

    #[test]
    fn rejects_svg_documents() {
        assert_eq!(
            inspect(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            Err(InvalidImage::Svg)
        );
        let xml = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        assert_eq!(inspect(xml), Err(InvalidImage::Svg));
        assert_eq!(
            inspect(b"\xef\xbb\xbf  <svg></svg>"),
            Err(InvalidImage::Svg)
        );
    }

    #[test]
    fn rejects_bogus_bytes_and_truncated_headers() {
        assert_eq!(
            inspect(b"not an image at all"),
            Err(InvalidImage::Unsupported)
        );
        assert_eq!(inspect(b"\x89PNG\r\n\x1a\n"), Err(InvalidImage::Truncated));
        assert_eq!(
            inspect(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]),
            Err(InvalidImage::Truncated)
        );
    }

    #[test]
    fn rejects_bytes_above_the_four_mib_cap() {
        let oversized = vec![0u8; MAX_UPLOAD_BYTES + 1];
        assert_eq!(
            inspect(&oversized),
            Err(InvalidImage::TooLarge {
                bytes: MAX_UPLOAD_BYTES + 1
            })
        );
    }

    #[test]
    fn keys_are_server_chosen_and_immutable() {
        assert_eq!(
            cover_key("ohio-senate-2026", "0b7c", ImageFormat::Jpeg),
            "cover/ohio-senate-2026/0b7c.jpg"
        );
        assert_eq!(
            cover_key("ohio-senate-2026", "0b7c", ImageFormat::WebP),
            "cover/ohio-senate-2026/0b7c.webp"
        );
        assert_eq!(CACHE_CONTROL, "public, max-age=31536000, immutable");
        assert_eq!(ImageFormat::Png.content_type(), "image/png");
    }
}
