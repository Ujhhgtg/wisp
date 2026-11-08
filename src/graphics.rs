use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use libghostty_vt::alloc::{Allocator, Bytes};
use libghostty_vt::kitty::graphics::{
    Compression, DecodePng, DecodedImage, Image, ImageFormat, PlacementIterator, set_png_decoder,
};
use libghostty_vt::{Error, Terminal};

use crate::config::Config;

mod animation;
pub mod placeholders;
mod protocol;

pub(super) const IMAGE_STORAGE_LIMIT: u64 = 64 * 1024 * 1024;
pub(super) const MAX_DECODED_BYTES: usize = IMAGE_STORAGE_LIMIT as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageLayer {
    BelowBackground,
    BelowText,
    AboveText,
}

impl ImageLayer {
    pub fn from_z(z: i32) -> Self {
        if z < i32::MIN / 2 {
            Self::BelowBackground
        } else if z < 0 {
            Self::BelowText
        } else {
            Self::AboveText
        }
    }
}

#[derive(Debug)]
pub struct ImagePixels {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub struct ImagePlacement {
    pub image: Arc<ImagePixels>,
    pub image_id: u32,
    pub placement_id: u32,
    pub z: i32,
    pub grid_position: [i32; 2],
    pub offset: [u32; 2],
    pub pixel_size: [u32; 2],
    pub source: [f32; 4],
}

impl PartialEq for ImagePlacement {
    fn eq(&self, other: &Self) -> bool {
        self.image.generation == other.image.generation
            && self.image_id == other.image_id
            && self.placement_id == other.placement_id
            && self.z == other.z
            && self.grid_position == other.grid_position
            && self.offset == other.offset
            && self.pixel_size == other.pixel_size
            && self.source == other.source
    }
}

#[derive(Clone, Debug, Default)]
pub struct GraphicsSnapshot {
    pub generation: u64,
    pub placements: Arc<[ImagePlacement]>,
}

pub struct GraphicsCapture {
    iterator: PlacementIterator<'static>,
    pixels: HashMap<u64, Arc<ImagePixels>>,
    previous: GraphicsSnapshot,
    protocol: protocol::Protocol,
}

impl GraphicsCapture {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            iterator: PlacementIterator::new()?,
            pixels: HashMap::new(),
            previous: GraphicsSnapshot::default(),
            protocol: protocol::Protocol::new(),
        })
    }

    pub fn write(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        data: &[u8],
        reply: &mut dyn FnMut(&[u8]),
    ) {
        self.protocol.write(terminal, config, data, reply);
    }

    pub fn advance(&mut self, terminal: &Terminal<'_, '_>) -> Result<bool, Error> {
        self.protocol.advance(terminal)
    }

    pub fn deadline(&self, terminal: &Terminal<'_, '_>) -> Option<Instant> {
        self.protocol.deadline(terminal)
    }

    pub fn update(
        &mut self,
        terminal: &Terminal<'_, '_>,
        config: &Config,
        placeholder_cells: &[placeholders::PlaceholderCell],
        cell_size: [u32; 2],
    ) -> Result<GraphicsSnapshot, Error> {
        if terminal.kitty_image_storage_limit()? == 0 {
            self.pixels.clear();
            self.previous = GraphicsSnapshot::default();
            return Ok(self.previous.clone());
        }
        let graphics = terminal.kitty_graphics()?;
        let generation = graphics.generation()?;
        if generation == 0 {
            self.pixels.clear();
            self.previous = GraphicsSnapshot::default();
            return Ok(self.previous.clone());
        }
        let mut placements = Vec::new();
        let mut virtuals = Vec::new();
        let mut seen = HashSet::new();
        let mut iteration = self.iterator.update(&graphics)?;
        while let Some(placement) = iteration.next() {
            let image_id = placement.image_id()?;
            let Some(image) = graphics.image(image_id) else {
                continue;
            };
            let image_generation = image.generation()?;
            seen.insert(image_generation);
            let pixels = if let Some(pixels) = self.protocol.pixels(terminal, &image, config)? {
                self.pixels.remove(&image_generation);
                pixels
            } else if let Some(pixels) = self.pixels.get(&image_generation) {
                pixels.clone()
            } else {
                let pixels = image_pixels(&image)?;
                self.pixels.insert(image_generation, pixels.clone());
                pixels
            };
            if placement.is_virtual()? {
                virtuals.push((
                    image_id,
                    placement.placement_id()?,
                    [placement.columns()?, placement.rows()?],
                    pixels,
                ));
                continue;
            }
            let info = placement.placement_render_info(&image, terminal)?;
            if !info.viewport_visible || info.pixel_width == 0 || info.pixel_height == 0 {
                continue;
            }
            let Some((offset, pixel_size)) = placement_geometry(
                [info.source_width, info.source_height],
                [placement.columns()?, placement.rows()?],
                [placement.x_offset()?, placement.y_offset()?],
                cell_size,
            ) else {
                continue;
            };
            placements.push(ImagePlacement {
                image: pixels,
                image_id,
                placement_id: placement.placement_id()?,
                z: placement.z()?,
                grid_position: [info.viewport_col, info.viewport_row],
                offset,
                pixel_size,
                source: [
                    info.source_x as f32,
                    info.source_y as f32,
                    info.source_width as f32,
                    info.source_height as f32,
                ],
            });
        }
        for run in placeholders::resolve(placeholder_cells) {
            let Some((_, placement_id, grid, image)) =
                virtuals.iter().find(|(image_id, placement_id, _, _)| {
                    *image_id == run.image_id
                        && (run.placement_id == 0 || *placement_id == run.placement_id)
                })
            else {
                continue;
            };
            let Some(geometry) =
                placeholders::geometry(&run, [image.width, image.height], *grid, cell_size)
            else {
                continue;
            };
            placements.push(ImagePlacement {
                image: image.clone(),
                image_id: run.image_id,
                placement_id: *placement_id,
                z: -1,
                grid_position: run.grid_position,
                offset: geometry.offset,
                pixel_size: geometry.pixel_size,
                source: geometry.source,
            });
        }
        self.protocol.set_visible(
            terminal,
            &placements
                .iter()
                .map(|placement| placement.image_id)
                .collect(),
        )?;
        self.pixels.retain(|stamp, _| seen.contains(stamp));
        placements
            .sort_by_key(|placement| (placement.z, placement.image_id, placement.placement_id));
        if generation == self.previous.generation
            && placements.as_slice() == self.previous.placements.as_ref()
        {
            return Ok(self.previous.clone());
        }
        self.previous = GraphicsSnapshot {
            generation,
            placements: placements.into(),
        };
        Ok(self.previous.clone())
    }
}

fn placement_geometry(
    source: [u32; 2],
    grid: [u32; 2],
    offset: [u32; 2],
    cells: [u32; 2],
) -> Option<([u32; 2], [u32; 2])> {
    if source.contains(&0) || cells.contains(&0) {
        return None;
    }
    let mut offset = [offset[0].min(cells[0] - 1), offset[1].min(cells[1] - 1)];
    let mut size = [f64::from(source[0]), f64::from(source[1])];
    match grid {
        [0, 0] => {}
        [columns, 0] => {
            let scale = f64::from(columns) * f64::from(cells[0]) / size[0];
            size[0] *= scale;
            size[1] *= scale;
        }
        [0, rows] => {
            let scale = f64::from(rows) * f64::from(cells[1]) / size[1];
            size[0] *= scale;
            size[1] *= scale;
        }
        [columns, rows] => {
            let bounds = [
                f64::from(columns) * f64::from(cells[0]),
                f64::from(rows) * f64::from(cells[1]),
            ];
            let scale = (bounds[0] / size[0]).min(bounds[1] / size[1]);
            size[0] *= scale;
            size[1] *= scale;
            offset[0] = offset[0].saturating_add(((bounds[0] - size[0]) / 2.0).round() as u32);
            offset[1] = offset[1].saturating_add(((bounds[1] - size[1]) / 2.0).round() as u32);
        }
    }
    Some((offset, [size[0].round() as u32, size[1].round() as u32]))
}

fn image_pixels(image: &Image<'_>) -> Result<Arc<ImagePixels>, Error> {
    if image.compression()? != Compression::None {
        return Err(Error::InvalidValue);
    }
    let width = image.width()?;
    let height = image.height()?;
    let rgba = normalize_pixels(width, height, image.format()?, image.data()?)
        .ok_or(Error::InvalidValue)?;
    Ok(Arc::new(ImagePixels {
        generation: image.generation()?,
        width,
        height,
        rgba: rgba.into(),
    }))
}

fn new_pixels(width: u32, height: u32, rgba: Arc<[u8]>) -> Arc<ImagePixels> {
    static GENERATION: AtomicU64 = AtomicU64::new(1 << 63);
    Arc::new(ImagePixels {
        generation: GENERATION.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        rgba,
    })
}

pub fn configure(terminal: &mut Terminal<'_, '_>, enabled: bool) -> Result<(), Error> {
    if enabled {
        set_png_decoder(Some(Box::new(PngDecoder { buffer: Vec::new() })))?;
        terminal
            .set_kitty_image_storage_limit(IMAGE_STORAGE_LIMIT)?
            .set_kitty_image_from_file_allowed(true)?
            .set_kitty_image_from_temp_file_allowed(true)?
            .set_kitty_image_from_shared_mem_allowed(true)?;
    } else {
        terminal.set_kitty_image_storage_limit(0)?;
    }
    Ok(())
}

fn normalize_pixels(width: u32, height: u32, format: ImageFormat, bytes: &[u8]) -> Option<Vec<u8>> {
    let pixels = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    let length = pixels.checked_mul(4)?;
    if pixels == 0 || length > MAX_DECODED_BYTES {
        return None;
    }
    let channels = match format {
        ImageFormat::Rgb => 3,
        ImageFormat::Rgba => 4,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
        _ => return None,
    };
    if bytes.len() != pixels.checked_mul(channels)? {
        return None;
    }
    if channels == 4 {
        return Some(bytes.to_vec());
    }
    let mut rgba = Vec::with_capacity(length);
    for pixel in bytes.chunks_exact(channels) {
        match format {
            ImageFormat::Rgb => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            ImageFormat::GrayAlpha => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]])
            }
            ImageFormat::Gray => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            _ => return None,
        }
    }
    Some(rgba)
}

struct PngDecoder {
    buffer: Vec<u8>,
}

impl DecodePng for PngDecoder {
    fn decode_png<'alloc>(
        &mut self,
        allocator: &'alloc Allocator<'_>,
        bytes: &[u8],
    ) -> Option<DecodedImage<'alloc>> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().ok()?;
        let length = reader.output_buffer_size()?;
        if length == 0 || length > MAX_DECODED_BYTES {
            return None;
        }
        self.buffer.resize(length, 0);
        let info = reader.next_frame(&mut self.buffer).ok()?;
        let format = match info.color_type {
            png::ColorType::Rgb => ImageFormat::Rgb,
            png::ColorType::Rgba => ImageFormat::Rgba,
            png::ColorType::Grayscale => ImageFormat::Gray,
            png::ColorType::GrayscaleAlpha => ImageFormat::GrayAlpha,
            png::ColorType::Indexed => return None,
        };
        let rgba = normalize_pixels(
            info.width,
            info.height,
            format,
            &self.buffer[..info.buffer_size()],
        )?;
        let mut data = Bytes::new_with_alloc(allocator, rgba.len()).ok()?;
        data.copy_from_slice(&rgba);
        Some(DecodedImage {
            width: info.width,
            height: info.height,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_boxes_preserve_aspect_ratio_and_center_letterbox_padding() {
        assert_eq!(
            placement_geometry([100, 50], [2, 2], [0, 0], [8, 16]),
            Some(([0, 12], [16, 8]))
        );
        assert_eq!(
            placement_geometry([50, 100], [8, 1], [0, 0], [8, 16]),
            Some(([28, 0], [8, 16]))
        );
        assert_eq!(
            placement_geometry([100, 50], [2, 0], [0, 0], [8, 16]),
            Some(([0, 0], [16, 8]))
        );
        assert_eq!(
            placement_geometry([100, 50], [0, 1], [0, 0], [8, 16]),
            Some(([0, 0], [32, 16]))
        );
    }

    #[test]
    fn natural_source_extents_and_cell_offsets_are_bounded() {
        assert_eq!(
            placement_geometry([2, 2], [0, 0], [999, 999], [8, 16]),
            Some(([7, 15], [2, 2]))
        );
        assert!(placement_geometry([0, 1], [1, 1], [0, 0], [8, 16]).is_none());
        assert!(placement_geometry([1, 1], [1, 1], [0, 0], [0, 16]).is_none());
    }
}
