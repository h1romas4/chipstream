//! Display-sized resampling for decoded track artwork.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui_kit::RenderImage;
use image::{Frame, RgbaImage, imageops::FilterType};

pub const ARTWORK_SIZE: f32 = 192.;

pub fn target_size(scale_factor: f32) -> u32 {
    (ARTWORK_SIZE * scale_factor).ceil().max(1.) as u32
}

pub fn resize(image: Arc<RenderImage>, target: u32) -> Result<Arc<RenderImage>> {
    let needs_resize = (0..image.frame_count()).any(|index| {
        let size = image.size(index);
        u32::from(size.width).max(u32::from(size.height)) > target
    });
    if !needs_resize {
        return Ok(image);
    }

    let mut frames = Vec::with_capacity(image.frame_count());
    for index in 0..image.frame_count() {
        let size = image.size(index);
        let width = u32::from(size.width);
        let height = u32::from(size.height);
        let bytes = image.as_bytes(index).context("missing artwork frame")?;
        let mut pixels =
            RgbaImage::from_raw(width, height, bytes.to_vec()).context("invalid artwork frame")?;
        let max_side = width.max(height);
        if max_side > target {
            // GPUI supplies BGRA, but premultiplication and filtering treat both color orders alike.
            for pixel in pixels.pixels_mut() {
                let alpha = u32::from(pixel[3]);
                for channel in &mut pixel.0[..3] {
                    *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
                }
            }
            let scaled = |side: u32| {
                ((u64::from(side) * u64::from(target) + u64::from(max_side) / 2)
                    / u64::from(max_side))
                .max(1) as u32
            };
            pixels = image::imageops::resize(
                &pixels,
                scaled(width),
                scaled(height),
                FilterType::Lanczos3,
            );
            for pixel in pixels.pixels_mut() {
                let alpha = u32::from(pixel[3]);
                if alpha != 0 {
                    for channel in &mut pixel.0[..3] {
                        *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                    }
                } else {
                    pixel.0[..3].fill(0);
                }
            }
        }
        frames.push(Frame::from_parts(pixels, 0, 0, image.delay(index)));
    }
    Ok(Arc::new(RenderImage::new(frames)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Rgba};

    fn render(pixels: RgbaImage) -> Arc<RenderImage> {
        Arc::new(RenderImage::new(vec![Frame::new(pixels)]))
    }

    #[test]
    fn target_tracks_display_density() {
        assert_eq!(target_size(1.), 192);
        assert_eq!(target_size(1.25), 240);
        assert_eq!(target_size(2.), 384);
    }

    #[test]
    fn keeps_small_images_and_preserves_aspect_ratio() {
        let small = render(RgbaImage::new(30, 20));
        assert!(Arc::ptr_eq(&small, &resize(small.clone(), 192).unwrap()));
        let large = render(RgbaImage::new(1200, 600));
        let resized = resize(large, 192).unwrap();
        assert_eq!(u32::from(resized.size(0).width), 192);
        assert_eq!(u32::from(resized.size(0).height), 96);
    }

    #[test]
    fn reduces_aliasing_of_fine_stripes() {
        let pixels = RgbaImage::from_fn(1024, 1024, |x, _| {
            let shade = if x % 2 == 0 { 0 } else { 255 };
            Rgba([shade, shade, shade, 255])
        });
        let resized = resize(render(pixels), 192).unwrap();
        let bytes = resized.as_bytes(0).unwrap();
        for x in 4..188 {
            let value = bytes[(96 * 192 + x) * 4];
            assert!((123..=132).contains(&value), "x={x}, value={value}");
        }
    }

    #[test]
    fn preserves_bgra_and_transparent_edges() {
        let pixels = RgbaImage::from_fn(400, 200, |x, _| {
            if x < 200 {
                Rgba([10, 20, 200, 255])
            } else {
                Rgba([255, 0, 0, 0])
            }
        });
        let resized = resize(render(pixels), 100).unwrap();
        let bytes = resized.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[10, 20, 200, 255]);
        for pixel in bytes.chunks_exact(4).filter(|pixel| pixel[3] > 16) {
            assert!(
                pixel[0] < 20 && pixel[1] < 30 && pixel[2] > 180,
                "{pixel:?}"
            );
        }
    }

    #[test]
    fn retains_animation_frames_and_delays() {
        let first = Frame::from_parts(
            RgbaImage::from_pixel(400, 200, Rgba([0, 0, 255, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(42, 1),
        );
        let second = Frame::from_parts(
            RgbaImage::from_pixel(400, 200, Rgba([255, 0, 0, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(84, 1),
        );
        let image = Arc::new(RenderImage::new(vec![first, second]));
        let resized = resize(image, 100).unwrap();
        assert_eq!(resized.frame_count(), 2);
        assert_eq!(resized.delay(0).numer_denom_ms(), (42, 1));
        assert_eq!(resized.delay(1).numer_denom_ms(), (84, 1));
        assert_eq!(&resized.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
        assert_eq!(&resized.as_bytes(1).unwrap()[..4], &[255, 0, 0, 255]);
    }
}
