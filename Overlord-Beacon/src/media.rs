//! Cross-platform image encoding helpers.

pub fn encode_jpeg(rgb: &[u8], width: i32, height: i32, quality: u8) -> Option<Vec<u8>> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(3)?;
    if rgb.len() < expected {
        return None;
    }
    let started = std::time::Instant::now();
    let encoded = libjpeg_turbo_rs::compress(
        &rgb[..expected],
        width as usize,
        height as usize,
        libjpeg_turbo_rs::PixelFormat::Rgb,
        quality.clamp(1, 100),
        libjpeg_turbo_rs::Subsampling::S420,
    )
    .ok()?;
    crate::util::perf_log(&format!(
        "jpeg {width}x{height} q{quality} {} bytes {:?}",
        encoded.len(),
        started.elapsed()
    ));
    Some(encoded)
}

pub fn encode_jpeg_bgra(bgra: &[u8], width: i32, height: i32, quality: u8) -> Option<Vec<u8>> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if bgra.len() < expected {
        return None;
    }
    let started = std::time::Instant::now();
    let encoded = libjpeg_turbo_rs::compress(
        &bgra[..expected],
        width as usize,
        height as usize,
        libjpeg_turbo_rs::PixelFormat::Bgra,
        quality.clamp(1, 100),
        libjpeg_turbo_rs::Subsampling::S420,
    )
    .ok()?;
    crate::util::perf_log(&format!(
        "jpeg-bgra {width}x{height} q{quality} {} bytes {:?}",
        encoded.len(),
        started.elapsed()
    ));
    Some(encoded)
}

pub fn encode_png(rgb: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(rgb).ok()?;
    }
    Some(out)
}
