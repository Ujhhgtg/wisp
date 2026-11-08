use libghostty_vt::mouse::EncoderSize;

use crate::config::Margins;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridGeometry {
    pub surface_width: u32,
    pub surface_height: u32,
    pub origin_x: u32,
    pub origin_y: u32,
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u32,
    pub cell_height: u32,
    pub content_width: u32,
    pub content_height: u32,
}

impl GridGeometry {
    pub fn new(
        surface_size: (u32, u32),
        cell_size: (u32, u32),
        scale_factor: f64,
        margins: Margins,
    ) -> Self {
        let surface_width = surface_size.0.max(1);
        let surface_height = surface_size.1.max(1);
        let cell_width = cell_size.0.max(1);
        let cell_height = cell_size.1.max(1);
        let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let (origin_x, cols, content_width) = axis_geometry(
            surface_width,
            cell_width,
            points_to_pixels(margins.left, scale_factor),
            points_to_pixels(margins.right, scale_factor),
        );
        let (origin_y, rows, content_height) = axis_geometry(
            surface_height,
            cell_height,
            points_to_pixels(margins.top, scale_factor),
            points_to_pixels(margins.bottom, scale_factor),
        );
        Self {
            surface_width,
            surface_height,
            origin_x,
            origin_y,
            cols,
            rows,
            cell_width,
            cell_height,
            content_width,
            content_height,
        }
    }

    pub const fn content_origin(self) -> (u32, u32) {
        (self.origin_x, self.origin_y)
    }

    pub const fn content_size(self) -> (u32, u32) {
        (self.content_width, self.content_height)
    }

    pub const fn terminal_size(self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    pub const fn cell_geometry(self) -> (u32, u32) {
        (self.cell_width, self.cell_height)
    }

    pub fn contains(self, x: f64, y: f64) -> bool {
        x.is_finite()
            && y.is_finite()
            && x >= f64::from(self.origin_x)
            && y >= f64::from(self.origin_y)
            && x < f64::from(self.origin_x + self.content_width)
            && y < f64::from(self.origin_y + self.content_height)
    }

    pub fn pixel_to_cell(self, x: f64, y: f64) -> Option<(u16, u16)> {
        self.contains(x, y)
            .then(|| self.pixel_to_cell_clamped(x, y))
    }

    pub fn pixel_to_cell_clamped(self, x: f64, y: f64) -> (u16, u16) {
        let column = ((x - f64::from(self.origin_x)) / f64::from(self.cell_width))
            .floor()
            .clamp(0.0, f64::from(self.cols - 1)) as u16;
        let row = ((y - f64::from(self.origin_y)) / f64::from(self.cell_height))
            .floor()
            .clamp(0.0, f64::from(self.rows - 1)) as u16;
        (column, row)
    }

    pub fn cell_origin(self, column: u16, row: u16) -> (u32, u32) {
        (
            self.origin_x + u32::from(column) * self.cell_width,
            self.origin_y + u32::from(row) * self.cell_height,
        )
    }

    pub fn mouse_size(self) -> EncoderSize {
        EncoderSize {
            screen_width: self.surface_width,
            screen_height: self.surface_height,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
            padding_top: self.origin_y,
            padding_bottom: self.surface_height - self.origin_y - self.content_height,
            padding_right: self.surface_width - self.origin_x - self.content_width,
            padding_left: self.origin_x,
        }
    }
}

fn points_to_pixels(points: f32, scale_factor: f64) -> u32 {
    if !points.is_finite() || points < 0.0 {
        return 0;
    }
    (f64::from(points) * (96.0 / 72.0) * scale_factor).round() as u32
}

fn axis_geometry(surface: u32, cell: u32, before: u32, after: u32) -> (u32, u16, u32) {
    let maximum_margin = surface.saturating_sub(cell.min(surface));
    let requested_margin = u64::from(before) + u64::from(after);
    let (before, after) = if requested_margin > u64::from(maximum_margin) {
        let before = (u64::from(before) * u64::from(maximum_margin) / requested_margin) as u32;
        (before, maximum_margin - before)
    } else {
        (before, after)
    };
    let available = surface - before - after;
    let cells = (available / cell).clamp(1, u32::from(u16::MAX)) as u16;
    let content = (u32::from(cells) * cell).min(available);
    let remainder = available - content;
    (before + remainder / 2, cells, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_margin_is_twenty_nine_physical_pixels_at_normal_scale() {
        let geometry = GridGeometry::new((778, 538), (8, 20), 1.0, Margins::default());
        assert_eq!(geometry.content_origin(), (29, 29));
        assert_eq!(geometry.content_size(), (720, 480));
        assert_eq!(geometry.terminal_size(), (90, 24));
        assert_eq!(geometry.mouse_size().padding_right, 29);
        assert_eq!(geometry.mouse_size().padding_bottom, 29);
    }

    #[test]
    fn point_margins_scale_with_the_same_dpi_as_cells() {
        let geometry = GridGeometry::new((1556, 1076), (16, 40), 2.0, Margins::default());
        assert_eq!(geometry.content_origin(), (58, 58));
        assert_eq!(geometry.terminal_size(), (90, 24));
        assert_eq!(geometry.content_size(), (1440, 960));
    }

    #[test]
    fn centered_remainder_is_included_in_mouse_padding() {
        let geometry = GridGeometry::new((783, 549), (8, 20), 1.0, Margins::default());
        assert_eq!(geometry.content_origin(), (31, 34));
        assert_eq!(geometry.content_size(), (720, 480));
        let mouse = geometry.mouse_size();
        assert_eq!((mouse.padding_left, mouse.padding_right), (31, 32));
        assert_eq!((mouse.padding_top, mouse.padding_bottom), (34, 35));
        assert_eq!(geometry.cell_origin(5, 2), (71, 74));
        assert_eq!(geometry.pixel_to_cell(71.0, 74.0), Some((5, 2)));
        assert_eq!(geometry.pixel_to_cell(70.99, 73.99), Some((4, 1)));
    }

    #[test]
    fn margin_and_padding_are_not_terminal_cells() {
        let geometry = GridGeometry::new((783, 549), (8, 20), 1.0, Margins::default());
        assert_eq!(geometry.pixel_to_cell(0.0, 0.0), None);
        assert_eq!(geometry.pixel_to_cell(750.99, 513.99), Some((89, 23)));
        assert_eq!(geometry.pixel_to_cell(751.0, 514.0), None);
        assert_eq!(geometry.pixel_to_cell_clamped(-1.0, -1.0), (0, 0));
        assert_eq!(geometry.pixel_to_cell_clamped(9999.0, 9999.0), (89, 23));
        assert!(!geometry.contains(f64::NAN, 30.0));
    }

    #[test]
    fn asymmetric_edges_keep_their_ratio_and_center_unused_pixels() {
        let margins = Margins {
            top: 3.0,
            right: 6.0,
            bottom: 9.0,
            left: 12.0,
        };
        let geometry = GridGeometry::new((104, 96), (8, 20), 1.0, margins);
        assert_eq!(geometry.content_origin(), (16, 4));
        assert_eq!(geometry.content_size(), (80, 80));
        assert_eq!(geometry.terminal_size(), (10, 4));
    }

    #[test]
    fn tiny_and_minimized_windows_keep_one_safe_clipped_cell() {
        for size in [(0, 0), (1, 1), (7, 5), (20, 10)] {
            let geometry = GridGeometry::new(size, (8, 20), 1.0, Margins::default());
            let mouse = geometry.mouse_size();
            assert_eq!(geometry.terminal_size(), (1, 1));
            assert!(geometry.content_width > 0 && geometry.content_height > 0);
            assert!(geometry.origin_x + geometry.content_width <= geometry.surface_width);
            assert!(geometry.origin_y + geometry.content_height <= geometry.surface_height);
            assert_eq!(
                mouse.padding_left + geometry.content_width + mouse.padding_right,
                geometry.surface_width
            );
            assert_eq!(
                mouse.padding_top + geometry.content_height + mouse.padding_bottom,
                geometry.surface_height
            );
        }
    }

    #[test]
    fn huge_margins_and_invalid_runtime_geometry_cannot_overflow() {
        let geometry = GridGeometry::new((100, 80), (0, 0), f64::NAN, Margins::uniform(f32::MAX));
        assert_eq!(geometry.terminal_size(), (1, 1));
        assert_eq!(geometry.content_size(), (1, 1));
        assert_eq!(geometry.pixel_to_cell_clamped(f64::NAN, f64::NAN), (0, 0));
        let zero = GridGeometry::new((16, 40), (8, 20), 1.0, Margins::uniform(f32::NAN));
        assert_eq!(zero.terminal_size(), (2, 2));
    }
}
