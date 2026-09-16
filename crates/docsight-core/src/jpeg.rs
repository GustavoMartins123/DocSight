use crate::{DecodedImage, DocsightError, MAX_IMAGE_PIXELS};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

pub const MAX_JPEG_SCANS: usize = 100;

pub fn decode_jpeg(bytes: &[u8]) -> Result<DecodedImage, DocsightError> {
    if !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Err(malformed_image("data does not start with a JPEG signature"));
    }
    let maximum_dimension = usize::try_from(MAX_IMAGE_PIXELS).map_err(|_| image_limit())?;
    let options = DecoderOptions::new_safe()
        .set_use_unsafe(false)
        .set_strict_mode(true)
        .set_max_width(maximum_dimension)
        .set_max_height(maximum_dimension)
        .jpeg_set_max_scans(MAX_JPEG_SCANS)
        .jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|error| malformed_image(error.to_string()))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| malformed_image("decoded headers have no dimensions"))?;
    let (width, height, expected_bytes) = validated_dimensions(width, height)?;
    let rgba = decoder
        .decode()
        .map_err(|error| malformed_image(error.to_string()))?;
    if rgba.len() != expected_bytes {
        return Err(malformed_image(
            "decoded pixels do not match the declared dimensions",
        ));
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn validated_dimensions(width: usize, height: usize) -> Result<(u32, u32, usize), DocsightError> {
    let pixels = width.checked_mul(height).ok_or_else(image_limit)?;
    if u64::try_from(pixels).map_err(|_| image_limit())? > MAX_IMAGE_PIXELS {
        return Err(image_limit());
    }
    let expected_bytes = pixels.checked_mul(4).ok_or_else(image_limit)?;
    Ok((
        u32::try_from(width).map_err(|_| image_limit())?,
        u32::try_from(height).map_err(|_| image_limit())?,
        expected_bytes,
    ))
}

fn malformed_image(message: impl Into<String>) -> DocsightError {
    DocsightError::MalformedDocument {
        message: format!("embedded JPEG image is malformed: {}", message.into()),
    }
}

fn image_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "JPEG pixels".to_owned(),
        limit: MAX_IMAGE_PIXELS,
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_IMAGE_PIXELS, decode_jpeg, validated_dimensions};
    use crate::DocsightError;

    #[test]
    fn rejects_non_jpeg_and_excessive_dimensions() {
        assert!(matches!(
            decode_jpeg(b"not a jpeg"),
            Err(DocsightError::MalformedDocument { .. })
        ));

        assert!(validated_dimensions(8_000, 5_000).is_ok());
        let result = validated_dimensions(8_000, 5_001);
        assert!(
            matches!(
                &result,
                Err(DocsightError::ResourceLimit {
                    resource,
                    limit: MAX_IMAGE_PIXELS
                }) if resource == "JPEG pixels"
            ),
            "{result:?}"
        );
    }
}
