use std::collections::HashMap;
use std::ops::Range;
#[cfg(unix)]
use std::process::Command;

use glyphon::cosmic_text::{FeatureTag, FontFeatures};
use glyphon::{
    Attrs, Buffer, CacheKey, Color, ContentType, CustomGlyph, Family, FontSystem, Metrics,
    RasterizedCustomGlyph, Shaping, SwashCache, SwashContent, SwashImage, Weight, Wrap,
};

use crate::config::Config;
use crate::terminal::{CellSnapshot, Rgb, TerminalSnapshot};

#[derive(Clone, Copy, Debug)]
pub(super) struct CellMetrics {
    pub width: u32,
    pub height: u32,
    pub baseline: f32,
    pub font_size: f32,
    pub underline_position: u32,
    pub underline_thickness: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RunStyle {
    bold: bool,
    italic: bool,
    hidden: bool,
}

#[derive(Clone, Debug)]
struct CellBytes {
    bytes: Range<usize>,
    column: u16,
    width: u8,
    foreground: Rgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GlyphRasterKey {
    cache_key: CacheKey,
    columns: [u32; 2],
}

#[derive(Default)]
struct CachedRow {
    cells: Vec<CellSnapshot>,
    glyphs: Vec<CustomGlyph>,
}

pub(super) struct TextLayout {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub metrics: CellMetrics,
    pub family: String,
    pub glyphs: Vec<CustomGlyph>,
    pub images: HashMap<u16, RasterizedCustomGlyph>,
    glyph_ids: HashMap<GlyphRasterKey, u16>,
    pub empty_buffer: Buffer,
    shaping_buffer: Buffer,
    rows: Vec<CachedRow>,
    pub revision: u64,
    pub shaped_rows: usize,
    ligatures: bool,
    selection_foreground: Rgb,
    blink_visible: bool,
}

impl TextLayout {
    pub fn new(config: &Config, scale_factor: f32) -> Result<Self, String> {
        let mut font_system = FontSystem::new();
        let family = resolve_family(&mut font_system, &config.font.family)?;
        let font_size = physical_font_size(config.font.size, scale_factor);
        let metrics = measure_font(&mut font_system, &family, font_size)?;
        let mut empty_buffer = Buffer::new(
            &mut font_system,
            Metrics::new(font_size, metrics.height as f32),
        );
        empty_buffer.set_text("", &Attrs::new(), Shaping::Advanced, None);
        Ok(Self {
            font_system,
            swash_cache: SwashCache::new(),
            metrics,
            family,
            glyphs: Vec::new(),
            images: HashMap::new(),
            glyph_ids: HashMap::new(),
            empty_buffer,
            shaping_buffer: Buffer::new_empty(Metrics::new(
                metrics.font_size,
                metrics.height as f32,
            )),
            rows: Vec::new(),
            revision: 0,
            shaped_rows: 0,
            ligatures: config.font.ligatures,
            selection_foreground: config.colors.selection_foreground.into(),
            blink_visible: true,
        })
    }

    pub fn set_blink_visible(&mut self, visible: bool) -> bool {
        if self.blink_visible == visible {
            return false;
        }
        self.blink_visible = visible;
        let mut changed = false;
        for row in &mut self.rows {
            if row.cells.iter().any(|cell| cell.blink && !cell.hidden) {
                row.cells.clear();
                changed = true;
            }
        }
        changed
    }

    pub fn shape(&mut self, snapshot: &TerminalSnapshot) -> Result<bool, String> {
        let reset_atlas = self.glyph_ids.len() > 60_000;
        if reset_atlas {
            self.glyph_ids.clear();
            self.images.clear();
            self.rows.clear();
        }
        let dimensions_changed = self.rows.len() != usize::from(snapshot.rows);
        self.rows
            .resize_with(usize::from(snapshot.rows), CachedRow::default);
        self.shaped_rows = 0;
        self.glyphs.clear();
        for row in 0..snapshot.rows {
            let start = usize::from(row) * usize::from(snapshot.cols);
            let cells = snapshot
                .cells
                .get(start..(start + usize::from(snapshot.cols)).min(snapshot.cells.len()))
                .unwrap_or_default();
            if self.rows[usize::from(row)].cells == cells {
                self.glyphs
                    .extend_from_slice(&self.rows[usize::from(row)].glyphs);
                continue;
            }
            let cache = &mut self.rows[usize::from(row)];
            if cache.cells.len() == cells.len()
                && cache.cells.iter().zip(cells).all(|(previous, current)| {
                    previous.text == current.text
                        && previous.width == current.width
                        && previous.foreground == current.foreground
                        && previous.bold == current.bold
                        && previous.italic == current.italic
                        && previous.blink == current.blink
                        && previous.hidden == current.hidden
                        && previous.selected == current.selected
                })
            {
                cache.cells.clone_from_slice(cells);
                self.glyphs.extend_from_slice(&cache.glyphs);
                continue;
            }
            self.shaped_rows += 1;
            let glyph_start = self.glyphs.len();
            let mut text = String::new();
            let mut mapping = Vec::new();
            let mut style = None;
            for (column, cell) in cells.iter().enumerate() {
                if cell.width == 0 {
                    continue;
                }
                let cell_style = RunStyle {
                    bold: cell.bold,
                    italic: cell.italic,
                    hidden: cell.hidden || (cell.blink && !self.blink_visible),
                };
                if style.is_some_and(|previous| previous != cell_style) {
                    self.shape_run(row, &text, &mapping, style.expect("run style"))?;
                    text.clear();
                    mapping.clear();
                }
                style = Some(cell_style);
                let begin = text.len();
                if cell.text.is_empty() || cell_style.hidden {
                    text.push(' ');
                } else {
                    text.push_str(&cell.text);
                }
                mapping.push(CellBytes {
                    bytes: begin..text.len(),
                    column: column as u16,
                    width: cell.width,
                    foreground: if cell.selected {
                        self.selection_foreground
                    } else {
                        cell.foreground
                    },
                });
            }
            if let Some(style) = style {
                self.shape_run(row, &text, &mapping, style)?;
            }
            let cache = &mut self.rows[usize::from(row)];
            cache.cells.clear();
            cache.cells.extend_from_slice(cells);
            cache.glyphs.clear();
            cache.glyphs.extend_from_slice(&self.glyphs[glyph_start..]);
        }
        if dimensions_changed || self.shaped_rows > 0 || reset_atlas {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(reset_atlas)
    }

    fn shape_run(
        &mut self,
        row: u16,
        text: &str,
        mapping: &[CellBytes],
        style: RunStyle,
    ) -> Result<(), String> {
        let mut features = FontFeatures::new();
        if !self.ligatures {
            features
                .disable(FeatureTag::STANDARD_LIGATURES)
                .disable(FeatureTag::CONTEXTUAL_LIGATURES)
                .disable(FeatureTag::CONTEXTUAL_ALTERNATES)
                .disable(FeatureTag::DISCRETIONARY_LIGATURES);
        }
        let attrs = Attrs::new()
            .family(Family::Name(&self.family))
            .weight(if style.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            })
            .style(if style.italic {
                glyphon::Style::Italic
            } else {
                glyphon::Style::Normal
            })
            .font_features(features);
        let buffer = &mut self.shaping_buffer;
        let mut colors = Vec::new();
        for cell in mapping {
            if colors
                .last()
                .is_none_or(|(_, color)| *color != cell.foreground)
            {
                colors.push((
                    f32::from(cell.column) * self.metrics.width as f32,
                    cell.foreground,
                ));
            }
        }
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        for run in buffer.layout_runs() {
            let mut cluster_extents: HashMap<(usize, usize), (f32, f32)> = HashMap::new();
            for glyph in run.glyphs {
                let extent = cluster_extents
                    .entry((glyph.start, glyph.end))
                    .or_insert((glyph.x, glyph.x + glyph.w));
                extent.0 = extent.0.min(glyph.x);
                extent.1 = extent.1.max(glyph.x + glyph.w);
            }
            for glyph in run.glyphs {
                let Some(first) =
                    mapping.get(mapping.partition_point(|cell| cell.bytes.end <= glyph.start))
                else {
                    continue;
                };
                let last = mapping
                    .get(
                        mapping
                            .partition_point(|cell| cell.bytes.start < glyph.end)
                            .saturating_sub(1),
                    )
                    .unwrap_or(first);
                let (cluster_left, cluster_right) = cluster_extents[&(glyph.start, glyph.end)];
                let cluster_width = (u32::from(last.column) + u32::from(last.width)
                    - u32::from(first.column)) as f32
                    * self.metrics.width as f32;
                let cluster_offset = (cluster_width - (cluster_right - cluster_left)) * 0.5;
                let offset_x =
                    first.column as f32 * self.metrics.width as f32 + cluster_offset - cluster_left;
                let baseline = row as f32 * self.metrics.height as f32 + self.metrics.baseline;
                let physical = glyph.physical((offset_x, 0.0), 1.0);
                let Some(image) = self
                    .swash_cache
                    .get_image(&mut self.font_system, physical.cache_key)
                    .as_ref()
                else {
                    continue;
                };
                if image.placement.width == 0 || image.placement.height == 0 {
                    continue;
                }
                let left = (physical.x + image.placement.left) as f32;
                let mut color_index = colors
                    .partition_point(|(start, _)| *start <= left)
                    .saturating_sub(1);
                let mut start = 0;
                while start < image.placement.width {
                    let end = if image.content == SwashContent::Color {
                        image.placement.width
                    } else {
                        colors.get(color_index + 1).map_or(
                            image.placement.width,
                            |(boundary, _)| {
                                (*boundary - left)
                                    .round()
                                    .clamp(0.0, image.placement.width as f32)
                                    as u32
                            },
                        )
                    };
                    if end > start {
                        let key = GlyphRasterKey {
                            cache_key: physical.cache_key,
                            columns: [start, end],
                        };
                        let glyph_id =
                            cache_glyph(&mut self.glyph_ids, &mut self.images, key, image)?;
                        let foreground = colors[color_index].1;
                        self.glyphs.push(CustomGlyph {
                            id: glyph_id,
                            left: left + start as f32,
                            top: baseline.round() + physical.y as f32 - image.placement.top as f32,
                            width: (end - start) as f32,
                            height: image.placement.height as f32,
                            color: Some(Color::rgb(
                                foreground.red,
                                foreground.green,
                                foreground.blue,
                            )),
                            snap_to_physical_pixel: true,
                            metadata: usize::from(row) * usize::from(u16::MAX)
                                + usize::from(first.column),
                        });
                    }
                    start = end;
                    color_index += 1;
                }
            }
        }
        Ok(())
    }
}

fn cache_glyph(
    glyph_ids: &mut HashMap<GlyphRasterKey, u16>,
    images: &mut HashMap<u16, RasterizedCustomGlyph>,
    key: GlyphRasterKey,
    image: &SwashImage,
) -> Result<u16, String> {
    if let Some(id) = glyph_ids.get(&key) {
        return Ok(*id);
    }
    let id = u16::try_from(glyph_ids.len())
        .map_err(|_| "glyph atlas ID capacity exceeded".to_owned())?;
    let (content_type, channels) = match image.content {
        SwashContent::Mask => (ContentType::Mask, 1),
        SwashContent::Color => (ContentType::Color, 4),
        SwashContent::SubpixelMask => {
            return Err("subpixel glyph rasterization is unsupported".to_owned());
        }
    };
    let [start, end] = key.columns;
    let data = if start == 0 && end == image.placement.width {
        image.data.clone()
    } else {
        let mut cropped =
            Vec::with_capacity((end - start) as usize * image.placement.height as usize * channels);
        for row in image
            .data
            .chunks_exact(image.placement.width as usize * channels)
        {
            cropped.extend_from_slice(&row[start as usize * channels..end as usize * channels]);
        }
        cropped
    };
    images.insert(id, RasterizedCustomGlyph { data, content_type });
    glyph_ids.insert(key, id);
    Ok(id)
}

fn resolve_family(font_system: &mut FontSystem, requested: &str) -> Result<String, String> {
    let requested = requested.trim();
    if !requested.eq_ignore_ascii_case("monospace")
        && let Some(family) = font_system
            .db()
            .faces()
            .flat_map(|face| &face.families)
            .find(|(family, _)| family.eq_ignore_ascii_case(requested))
            .map(|(family, _)| family.clone())
    {
        return Ok(family);
    }
    #[cfg(unix)]
    {
        if let Ok(output) = Command::new("fc-match")
            .args(["--format", "%{family[0]}", requested])
            .output()
            && output.status.success()
        {
            let matched = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if font_system
                .db()
                .faces()
                .any(|face| face.families.iter().any(|(family, _)| *family == matched))
            {
                return Ok(matched);
            }
        }
    }
    let fallback = font_system
        .db()
        .faces()
        .find(|face| face.monospaced)
        .and_then(|face| face.families.first())
        .map(|(family, _)| family.clone());
    fallback.ok_or_else(|| "no usable monospace font is installed".to_owned())
}

fn measure_font(
    font_system: &mut FontSystem,
    family: &str,
    font_size: f32,
) -> Result<CellMetrics, String> {
    let attrs = Attrs::new().family(Family::Name(family));
    let mut probe = Buffer::new(font_system, Metrics::new(font_size, font_size * 2.0));
    probe.set_text("M", &attrs, Shaping::Advanced, None);
    probe.shape_until_scroll(font_system, false);
    let glyph = probe
        .layout_runs()
        .flat_map(|run| run.glyphs)
        .next()
        .ok_or_else(|| format!("font {family:?} produced no glyphs"))?;
    let width = glyph.w.round().max(1.0) as u32;
    let font = font_system
        .get_font(glyph.font_id, Weight::NORMAL)
        .ok_or_else(|| format!("failed to load font {family:?}"))?;
    let metrics = font.metrics();
    let scale = font_size / f32::from(metrics.units_per_em.max(1));
    let ascent = metrics.ascent * scale;
    let descent = -metrics.descent * scale;
    let leading = (metrics.leading * scale).max(0.0);
    let height = (ascent + descent + leading).ceil().max(1.0) as u32;
    let baseline = ((height as f32 - ascent - descent) * 0.5 + ascent).round();
    let (underline_position, underline_thickness) = metrics.underline.map_or_else(
        || {
            (
                (baseline + 1.0).min(height.saturating_sub(1) as f32) as u32,
                (font_size / 14.0).round().max(1.0) as u32,
            )
        },
        |underline| {
            (
                (baseline - underline.offset * scale)
                    .ceil()
                    .clamp(0.0, height.saturating_sub(1) as f32) as u32,
                (underline.thickness * scale).ceil().max(1.0) as u32,
            )
        },
    );
    Ok(CellMetrics {
        width,
        height,
        baseline,
        font_size,
        underline_position,
        underline_thickness,
    })
}

pub(super) fn physical_font_size(points: f32, scale_factor: f32) -> f32 {
    let points = if points.is_finite() {
        points.max(1.0)
    } else {
        11.0
    };
    let scale_factor = if scale_factor.is_finite() {
        scale_factor.max(0.5)
    } else {
        1.0
    };
    (points * (96.0 / 72.0) * scale_factor).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{CellSnapshot, CursorSnapshot};

    fn snapshot(config: &Config, columns: u16, lines: &[&str]) -> TerminalSnapshot {
        let mut cells = vec![CellSnapshot::default(); usize::from(columns) * lines.len()];
        for (row, line) in lines.iter().enumerate() {
            for (column, character) in line.chars().take(usize::from(columns)).enumerate() {
                cells[row * usize::from(columns) + column] = CellSnapshot {
                    text: character.to_string(),
                    foreground: config.colors.foreground.into(),
                    ..Default::default()
                };
            }
        }
        TerminalSnapshot {
            cols: columns,
            rows: lines.len() as u16,
            cells,
            background: config.colors.background.into(),
            foreground: config.colors.foreground.into(),
            cursor_color: config.colors.cursor.into(),
            cursor: CursorSnapshot::default(),
            mouse_reporting: false,
            graphics: Default::default(),
        }
    }

    #[test]
    fn installed_family_and_actual_advance_are_used() {
        let mut config = Config::default();
        let family = {
            let font_system = FontSystem::new();
            font_system
                .db()
                .faces()
                .find(|face| face.monospaced)
                .and_then(|face| face.families.first())
                .map(|(family, _)| family.clone())
                .expect("an installed monospace family")
        };
        config.font.family = family.clone();
        let layout = TextLayout::new(&config, 1.0).expect("font should load");
        assert_eq!(layout.family, family);
        let mut probe = Buffer::new_empty(Metrics::new(
            layout.metrics.font_size,
            layout.metrics.height as f32,
        ));
        probe.set_text(
            "M",
            &Attrs::new().family(Family::Name(&layout.family)),
            Shaping::Advanced,
            None,
        );
        let mut font_system = FontSystem::new();
        probe.shape_until_scroll(&mut font_system, false);
        let advance = probe.layout_runs().next().expect("layout run").glyphs[0].w;
        assert_eq!(layout.metrics.width, advance.round() as u32);
        assert!((layout.metrics.font_size - 14.666_667).abs() < 0.001);
    }

    #[test]
    fn physical_font_size_scales_points_and_dpi() {
        assert!((physical_font_size(11.0, 2.0) - 29.333_334).abs() < 0.001);
        assert!(physical_font_size(f32::NAN, f32::NAN).is_finite());
    }

    #[test]
    fn coding_ligatures_apply_without_collapsing_following_cells() {
        let mut config = Config::default();
        config.font.family = "FiraCode Nerd Font".to_owned();
        let mut enabled = TextLayout::new(&config, 1.0).expect("coding font should load");
        assert_eq!(enabled.family, "FiraCode Nerd Font");
        config.font.ligatures = false;
        let mut disabled = TextLayout::new(&config, 1.0).expect("coding font should load");
        let text = "M->=>!=M";
        let snapshot = TerminalSnapshot {
            cols: text.len() as u16,
            rows: 1,
            cells: text
                .chars()
                .map(|character| CellSnapshot {
                    text: character.to_string(),
                    foreground: config.colors.foreground.into(),
                    ..Default::default()
                })
                .collect(),
            background: config.colors.background.into(),
            foreground: config.colors.foreground.into(),
            cursor_color: config.colors.cursor.into(),
            cursor: CursorSnapshot::default(),
            mouse_reporting: false,
            graphics: Default::default(),
        };
        enabled.shape(&snapshot).expect("ligature run should shape");
        disabled.shape(&snapshot).expect("plain run should shape");
        let mut enabled_ids = enabled
            .glyph_ids
            .keys()
            .map(|key| key.cache_key.glyph_id)
            .collect::<Vec<_>>();
        let mut disabled_ids = disabled
            .glyph_ids
            .keys()
            .map(|key| key.cache_key.glyph_id)
            .collect::<Vec<_>>();
        enabled_ids.sort_unstable();
        disabled_ids.sort_unstable();
        assert_ne!(
            enabled_ids, disabled_ids,
            "ligature configuration must change actual OpenType glyph substitutions"
        );
        let first = enabled
            .glyphs
            .iter()
            .find(|glyph| glyph.metadata == 0)
            .expect("first M glyph");
        let last = enabled
            .glyphs
            .iter()
            .find(|glyph| glyph.metadata == text.len() - 1)
            .expect("last M glyph");
        assert!(
            (last.left - first.left - (text.len() - 1) as f32 * enabled.metrics.width as f32).abs()
                < 0.001
        );
    }

    #[test]
    fn color_and_selection_boundaries_preserve_ligature_substitutions() {
        let mut config = Config::default();
        config.font.family = "FiraCode Nerd Font".to_owned();
        let mut uniform = TextLayout::new(&config, 1.0).unwrap();
        let mut colored = TextLayout::new(&config, 1.0).unwrap();
        let mut selected = TextLayout::new(&config, 1.0).unwrap();
        let mut frame = snapshot(&config, 20, &["~> -> != === <=> M"]);
        uniform.shape(&frame).unwrap();
        for (index, cell) in frame.cells.iter_mut().enumerate() {
            if index % 2 == 0 {
                cell.foreground = Rgb {
                    red: 0,
                    green: 255,
                    blue: 0,
                };
            }
        }
        colored.shape(&frame).unwrap();
        for cell in &mut frame.cells {
            cell.foreground = config.colors.foreground.into();
        }
        frame.cells[0].selected = true;
        selected.shape(&frame).unwrap();
        let shaped_keys = |layout: &TextLayout| {
            layout
                .glyph_ids
                .keys()
                .map(|key| key.cache_key)
                .collect::<std::collections::HashSet<_>>()
        };
        assert_eq!(shaped_keys(&uniform), shaped_keys(&colored));
        assert_eq!(shaped_keys(&uniform), shaped_keys(&selected));
        let entries = colored.glyph_ids.len();
        for (index, cell) in frame.cells.iter_mut().enumerate() {
            cell.foreground = if index % 2 == 0 {
                Rgb {
                    red: 255,
                    green: 0,
                    blue: 0,
                }
            } else {
                Rgb {
                    red: 0,
                    green: 0,
                    blue: 255,
                }
            };
        }
        frame.cells[0].selected = false;
        colored.shape(&frame).unwrap();
        assert_eq!(
            colored.glyph_ids.len(),
            entries,
            "recoloring must reuse rasterized glyphs"
        );
    }

    #[test]
    fn cursor_only_snapshots_reuse_all_shaped_rows_and_atlas_entries() {
        let config = Config::default();
        let mut layout = TextLayout::new(&config, 1.0).unwrap();
        let mut snapshot = snapshot(&config, 16, &["alpha", "beta", "gamma"]);
        layout.shape(&snapshot).unwrap();
        assert_eq!(layout.shaped_rows, 3);
        let revision = layout.revision;
        let atlas_entries = layout.glyph_ids.len();
        let glyphs = layout.glyphs.clone();
        snapshot.cursor = CursorSnapshot {
            column: 8,
            row: 1,
            visible: true,
            blinking: true,
            ..Default::default()
        };
        layout.shape(&snapshot).unwrap();
        assert_eq!(layout.shaped_rows, 0);
        assert_eq!(layout.revision, revision);
        assert_eq!(layout.glyph_ids.len(), atlas_entries);
        assert_eq!(layout.glyphs, glyphs);
    }

    #[test]
    fn background_and_decoration_changes_reuse_shaping_and_keep_cached_attributes_current() {
        let config = Config::default();
        let mut layout = TextLayout::new(&config, 1.0).unwrap();
        let mut frame = snapshot(&config, 16, &["alpha", "beta", "gamma"]);
        layout.shape(&frame).unwrap();
        let revision = layout.revision;
        let glyphs = layout.glyphs.clone();
        let atlas_entries = layout.glyph_ids.len();
        let cell = &mut frame.cells[0];
        cell.background = Some(Rgb {
            red: 10,
            green: 20,
            blue: 30,
        });
        cell.underline = true;
        cell.underline_style = crate::terminal::UnderlineStyle::Curly;
        cell.underline_color = Some(Rgb {
            red: 40,
            green: 50,
            blue: 60,
        });
        cell.strikethrough = true;
        cell.overline = true;
        layout.shape(&frame).unwrap();
        assert_eq!(layout.shaped_rows, 0);
        assert_eq!(layout.revision, revision);
        assert_eq!(layout.glyph_ids.len(), atlas_entries);
        assert_eq!(layout.glyphs, glyphs);
        assert_eq!(layout.rows[0].cells, frame.cells[..16]);
        frame.cells[0].foreground = Rgb {
            red: 255,
            green: 0,
            blue: 0,
        };
        layout.shape(&frame).unwrap();
        assert_eq!(layout.shaped_rows, 1);
        assert_ne!(layout.glyphs, glyphs);
        frame.cells[0].blink = true;
        layout.shape(&frame).unwrap();
        assert!(layout.set_blink_visible(false));
        layout.shape(&frame).unwrap();
        assert_eq!(layout.shaped_rows, 1);
    }

    #[test]
    fn one_changed_row_preserves_other_rows_and_advances_one_revision() {
        let config = Config::default();
        let mut layout = TextLayout::new(&config, 1.0).unwrap();
        let mut snapshot = snapshot(&config, 16, &["alpha", "beta", "gamma"]);
        layout.shape(&snapshot).unwrap();
        let first = layout.rows[0].glyphs.clone();
        let last = layout.rows[2].glyphs.clone();
        let revision = layout.revision;
        snapshot.cells[16].text = "z".to_owned();
        layout.shape(&snapshot).unwrap();
        assert_eq!(layout.shaped_rows, 1);
        assert_eq!(layout.revision, revision + 1);
        assert_eq!(layout.rows[0].glyphs, first);
        assert_eq!(layout.rows[2].glyphs, last);
        assert_eq!(layout.rows[1].cells[0].text, "z");
        layout.shape(&snapshot).unwrap();
        assert_eq!(layout.shaped_rows, 0);
        assert_eq!(layout.revision, revision + 1);
    }

    #[test]
    fn reused_buffer_preserves_wide_cluster_anchors_and_glyph_boundaries() {
        let mut config = Config::default();
        config.font.family = "FiraCode Nerd Font".to_owned();
        let mut layout = TextLayout::new(&config, 1.0).unwrap();
        let mut snapshot = snapshot(&config, 24, &["M? e?=>M", "warm up shaping"]);
        snapshot.cells[1].text = "漢".to_owned();
        snapshot.cells[1].width = 2;
        snapshot.cells[2].text.clear();
        snapshot.cells[2].width = 0;
        snapshot.cells[3].text = "e\u{301}".to_owned();
        snapshot.cells[4].text = "=".to_owned();
        snapshot.cells[5].text = ">".to_owned();
        snapshot.cells[6].text = "M".to_owned();
        snapshot.cells[7].text.clear();
        snapshot.cells[20].text = "M".to_owned();
        layout.shape(&snapshot).unwrap();
        let first = layout
            .glyphs
            .iter()
            .find(|glyph| glyph.metadata == 0)
            .unwrap();
        for column in [6, 20] {
            let glyph = layout
                .glyphs
                .iter()
                .find(|glyph| glyph.metadata == column)
                .unwrap();
            assert_eq!(
                glyph.left - first.left,
                column as f32 * layout.metrics.width as f32
            );
            assert_eq!(glyph.id, first.id);
        }
        let arrow_end = layout
            .glyphs
            .iter()
            .filter(|glyph| glyph.metadata == 4 || glyph.metadata == 5)
            .map(|glyph| glyph.left + glyph.width)
            .fold(0.0, f32::max);
        assert!(arrow_end <= 6.0 * layout.metrics.width as f32);
        snapshot.cells[24].text = "M".to_owned();
        layout.shape(&snapshot).unwrap();
        assert_eq!(layout.shaped_rows, 1);
    }
}
