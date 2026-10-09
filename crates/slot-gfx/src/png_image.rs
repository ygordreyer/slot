use std::path::Path;

pub(crate) struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub(crate) fn load(path: &Path) -> Result<Image, String> {
    decode(path, false).map_err(|e| format!("{}: {e}", path.display()))
}

fn decode(path: &Path, native_overlay: bool) -> Result<Image, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = png::Decoder::new(file);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let header = reader.info();
    if native_overlay && (header.width, header.height) != (crate::OUT_W, crate::OUT_H) {
        return Err(format!(
            "native overlay must be 720x480, got {}x{}",
            header.width, header.height
        ));
    }
    let mut bytes = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut bytes).map_err(|e| e.to_string())?;
    let mut rgba = Vec::with_capacity(info.width as usize * info.height as usize * 4);
    for pixel in bytes[..info.buffer_size()].chunks(info.color_type.samples()) {
        match info.color_type {
            png::ColorType::Rgba => rgba.extend_from_slice(pixel),
            png::ColorType::Rgb => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            png::ColorType::Grayscale => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255])
            }
            png::ColorType::GrayscaleAlpha => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]])
            }
            _ => return Err("unsupported PNG format".into()),
        }
    }
    Ok(Image {
        width: info.width,
        height: info.height,
        rgba,
    })
}

pub(crate) fn overlay(path: &Path) -> Result<Image, String> {
    decode(path, true).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_size_overlay_is_rejected_before_decoding_incomplete_image_data() {
        let path = std::env::temp_dir().join(format!(
            "slot-png-incomplete-overlay-{}.png",
            std::process::id()
        ));
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_chunk(png::chunk::IDAT, &[0x78, 0x9c]).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
        assert!(load(&path).is_err(), "the image data must be incomplete");
        assert_eq!(
            overlay(&path).err().unwrap(),
            format!(
                "{}: native overlay must be 720x480, got 1x1",
                path.display()
            )
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn generated_pngs_decode_alpha_and_reject_missing_or_non_native_overlays() {
        let dir = std::env::temp_dir().join(format!("slot-png-profile-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        for (name, width, height, colour, pixel, expected) in [
            (
                "native.png",
                720,
                480,
                png::ColorType::Rgba,
                vec![240, 40, 80, 128],
                vec![240, 40, 80, 128],
            ),
            (
                "rgb.png",
                1,
                1,
                png::ColorType::Rgb,
                vec![10, 20, 30],
                vec![10, 20, 30, 255],
            ),
            (
                "grey.png",
                1,
                1,
                png::ColorType::GrayscaleAlpha,
                vec![45, 90],
                vec![45, 45, 45, 90],
            ),
        ] {
            let path = dir.join(name);
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = png::Encoder::new(file, width, height);
            encoder.set_color(colour);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&pixel.repeat((width * height) as usize))
                .unwrap();
            let image = load(&path).unwrap();
            assert_eq!((image.width, image.height), (width, height));
            assert_eq!(image.rgba, expected.repeat((width * height) as usize));
            if width == 720 {
                assert!(overlay(&path).is_ok());
            } else {
                let error = overlay(&path).err().unwrap();
                assert!(error.contains("native overlay must be 720x480"), "{error}");
            }
            std::fs::remove_file(path).unwrap();
        }
        assert!(overlay(&dir.join("missing.png"))
            .err()
            .unwrap()
            .contains("missing.png"));
        std::fs::remove_dir(dir).unwrap();
    }
}
