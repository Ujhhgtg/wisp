use libghostty_vt::style::StyleColor;

pub const PLACEHOLDER: char = '\u{10eeee}';

#[derive(Clone, Copy, Debug)]
pub struct PlaceholderCell {
    column: u16,
    row: u16,
    foreground: StyleColor,
    underline: StyleColor,
    image_row: Option<u32>,
    image_column: Option<u32>,
    image_high: Option<u8>,
}

impl PlaceholderCell {
    pub fn from_text(
        column: u16,
        row: u16,
        text: &str,
        foreground: StyleColor,
        underline: StyleColor,
    ) -> Option<Self> {
        let mut characters = text.chars();
        if characters.next()? != PLACEHOLDER {
            return None;
        }
        Some(Self {
            column,
            row,
            foreground,
            underline,
            image_row: characters.next().and_then(diacritic_index),
            image_column: characters.next().and_then(diacritic_index),
            image_high: characters
                .next()
                .and_then(diacritic_index)
                .and_then(|index| u8::try_from(index).ok()),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaceholderRun {
    pub image_id: u32,
    pub placement_id: u32,
    pub grid_position: [i32; 2],
    pub image_position: [u32; 2],
    pub cell_size: [u32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaceholderGeometry {
    pub offset: [u32; 2],
    pub pixel_size: [u32; 2],
    pub source: [f32; 4],
}

pub fn resolve(cells: &[PlaceholderCell]) -> Vec<PlaceholderRun> {
    let mut runs: Vec<PlaceholderRun> = Vec::new();
    let mut previous: Option<(PlaceholderCell, [u32; 2], u8)> = None;
    for cell in cells {
        let compatible = previous.filter(|(last, _, _)| {
            last.row == cell.row
                && u32::from(last.column) + 1 == u32::from(cell.column)
                && last.foreground == cell.foreground
                && last.underline == cell.underline
        });
        let mut image_row = cell.image_row.unwrap_or(0);
        let mut image_column = cell.image_column.unwrap_or(0);
        let mut image_high = cell.image_high.unwrap_or(0);
        if let Some((_, last_position, last_high)) = compatible {
            if cell.image_row.is_none() {
                image_row = last_position[1];
                image_column = last_position[0] + 1;
                image_high = last_high;
            } else if image_row == last_position[1] {
                if cell.image_column.is_none() {
                    image_column = last_position[0] + 1;
                    image_high = last_high;
                } else if image_column == last_position[0] + 1 && cell.image_high.is_none() {
                    image_high = last_high;
                }
            }
        }
        let image_id = color_id(cell.foreground) | (u32::from(image_high) << 24);
        let placement_id = color_id(cell.underline);
        if let Some(run) = runs.last_mut().filter(|run| {
            run.image_id == image_id
                && run.placement_id == placement_id
                && run.grid_position[1] == i32::from(cell.row)
                && run.grid_position[0] + run.cell_size[0] as i32 == i32::from(cell.column)
                && run.image_position[1] == image_row
                && run.image_position[0] + run.cell_size[0] == image_column
        }) {
            run.cell_size[0] += 1;
        } else {
            runs.push(PlaceholderRun {
                image_id,
                placement_id,
                grid_position: [i32::from(cell.column), i32::from(cell.row)],
                image_position: [image_column, image_row],
                cell_size: [1, 1],
            });
        }
        previous = Some((*cell, [image_column, image_row], image_high));
    }
    runs
}

pub fn geometry(
    run: &PlaceholderRun,
    image_size: [u32; 2],
    placement_grid: [u32; 2],
    cell_pixels: [u32; 2],
) -> Option<PlaceholderGeometry> {
    if image_size.contains(&0) || cell_pixels.contains(&0) || run.cell_size.contains(&0) {
        return None;
    }
    let grid = [
        if placement_grid[0] == 0 {
            image_size[0].div_ceil(cell_pixels[0])
        } else {
            placement_grid[0]
        },
        if placement_grid[1] == 0 {
            image_size[1].div_ceil(cell_pixels[1])
        } else {
            placement_grid[1]
        },
    ];
    let grid_pixels = [
        f64::from(grid[0]) * f64::from(cell_pixels[0]),
        f64::from(grid[1]) * f64::from(cell_pixels[1]),
    ];
    let scale =
        (grid_pixels[0] / f64::from(image_size[0])).min(grid_pixels[1] / f64::from(image_size[1]));
    let image_origin = [
        (grid_pixels[0] - f64::from(image_size[0]) * scale) / 2.0,
        (grid_pixels[1] - f64::from(image_size[1]) * scale) / 2.0,
    ];
    let fragment_origin = [
        f64::from(run.image_position[0]) * f64::from(cell_pixels[0]),
        f64::from(run.image_position[1]) * f64::from(cell_pixels[1]),
    ];
    let fragment_size = [
        f64::from(run.cell_size[0]) * f64::from(cell_pixels[0]),
        f64::from(run.cell_size[1]) * f64::from(cell_pixels[1]),
    ];
    let left = fragment_origin[0].max(image_origin[0]);
    let top = fragment_origin[1].max(image_origin[1]);
    let right = (fragment_origin[0] + fragment_size[0])
        .min(image_origin[0] + f64::from(image_size[0]) * scale);
    let bottom = (fragment_origin[1] + fragment_size[1])
        .min(image_origin[1] + f64::from(image_size[1]) * scale);
    if left >= right || top >= bottom {
        return None;
    }
    let pixel_size = [(right - left).round() as u32, (bottom - top).round() as u32];
    if pixel_size.contains(&0) {
        return None;
    }
    Some(PlaceholderGeometry {
        offset: [
            (left - fragment_origin[0]).round() as u32,
            (top - fragment_origin[1]).round() as u32,
        ],
        pixel_size,
        source: [
            ((left - image_origin[0]) / scale) as f32,
            ((top - image_origin[1]) / scale) as f32,
            ((right - left) / scale) as f32,
            ((bottom - top) / scale) as f32,
        ],
    })
}

fn color_id(color: StyleColor) -> u32 {
    match color {
        StyleColor::None => 0,
        StyleColor::Palette(index) => u32::from(index.0),
        StyleColor::Rgb(color) => {
            (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
        }
    }
}

fn diacritic_index(character: char) -> Option<u32> {
    DIACRITICS
        .binary_search(&(character as u32))
        .ok()
        .map(|index| index as u32)
}

const DIACRITICS: &[u32] = &[
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::style::{PaletteIndex, RgbColor};

    fn cell(column: u16, row: u16, diacritics: &[u32]) -> PlaceholderCell {
        let mut text = PLACEHOLDER.to_string();
        text.extend(
            diacritics
                .iter()
                .map(|index| char::from_u32(DIACRITICS[*index as usize]).unwrap()),
        );
        PlaceholderCell::from_text(
            column,
            row,
            &text,
            StyleColor::Palette(PaletteIndex(42)),
            StyleColor::None,
        )
        .unwrap()
    }

    #[test]
    fn all_diacritic_indices_match_the_protocol_table() {
        assert_eq!(DIACRITICS.len(), 297);
        assert!(DIACRITICS.windows(2).all(|pair| pair[0] < pair[1]));
        for (index, character) in DIACRITICS.iter().enumerate() {
            assert_eq!(
                diacritic_index(char::from_u32(*character).unwrap()),
                Some(index as u32)
            );
        }
    }

    #[test]
    fn explicit_cells_form_independent_row_runs() {
        let runs = resolve(&[
            cell(3, 4, &[0, 0]),
            cell(4, 4, &[0, 1]),
            cell(3, 5, &[1, 0]),
            cell(4, 5, &[1, 1]),
        ]);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].image_id, 42);
        assert_eq!(runs[0].grid_position, [3, 4]);
        assert_eq!(runs[0].image_position, [0, 0]);
        assert_eq!(runs[0].cell_size, [2, 1]);
        assert_eq!(runs[1].image_position, [0, 1]);
    }

    #[test]
    fn missing_diacritics_inherit_only_from_compatible_adjacent_cells() {
        let mut changed = cell(4, 0, &[]);
        changed.foreground = StyleColor::Rgb(RgbColor { r: 0, g: 0, b: 42 });
        let runs = resolve(&[
            cell(0, 0, &[2, 7, 3]),
            cell(1, 0, &[]),
            cell(2, 0, &[2]),
            cell(3, 0, &[2, 10]),
            changed,
            cell(6, 0, &[]),
            cell(0, 1, &[]),
        ]);
        assert_eq!(runs[0].image_id, 42 | (3 << 24));
        assert_eq!(runs[0].image_position, [7, 2]);
        assert_eq!(runs[0].cell_size, [4, 1]);
        assert_eq!(runs[1].image_id, 42);
        assert_eq!(runs[1].image_position, [0, 0]);
        assert_eq!(runs[2].image_position, [0, 0]);
        assert_eq!(runs[3].image_position, [0, 0]);
    }

    #[test]
    fn underline_color_encodes_the_placement_without_palette_resolution() {
        let mut first = cell(0, 0, &[0, 0]);
        first.underline = StyleColor::Rgb(RgbColor { r: 1, g: 2, b: 3 });
        let mut second = cell(1, 0, &[]);
        second.underline = StyleColor::Palette(PaletteIndex(7));
        let runs = resolve(&[first, second]);
        assert_eq!(runs[0].placement_id, 0x010203);
        assert_eq!(runs[1].placement_id, 7);
        assert_eq!(runs[1].image_position, [0, 0]);
    }

    #[test]
    fn invalid_diacritics_and_out_of_range_high_byte_do_not_shift_fields() {
        let text = format!("{PLACEHOLDER}a\u{30d}\u{1d244}");
        let parsed =
            PlaceholderCell::from_text(0, 0, &text, StyleColor::None, StyleColor::None).unwrap();
        let runs = resolve(&[parsed]);
        assert_eq!(runs[0].image_position, [1, 0]);
        assert_eq!(runs[0].image_id, 0);
    }

    #[test]
    fn aspect_fit_fragments_crop_the_image_and_preserve_letterbox_padding() {
        let mut run = resolve(&[cell(0, 0, &[0, 0])])[0];
        run.cell_size = [2, 1];
        let top = geometry(&run, [40, 20], [2, 2], [10, 10]).unwrap();
        assert_eq!(top.offset, [0, 5]);
        assert_eq!(top.pixel_size, [20, 5]);
        assert_eq!(top.source, [0.0, 0.0, 40.0, 10.0]);
        run.image_position[1] = 1;
        let bottom = geometry(&run, [40, 20], [2, 2], [10, 10]).unwrap();
        assert_eq!(bottom.offset, [0, 0]);
        assert_eq!(bottom.pixel_size, [20, 5]);
        assert_eq!(bottom.source, [0.0, 10.0, 40.0, 10.0]);
        run.image_position[1] = 2;
        assert!(geometry(&run, [40, 20], [2, 2], [10, 10]).is_none());
    }

    #[test]
    fn missing_grid_dimensions_use_the_natural_image_cell_extent() {
        let run = resolve(&[cell(0, 0, &[0, 0])])[0];
        let fragment = geometry(&run, [15, 10], [0, 0], [10, 10]).unwrap();
        assert_eq!(fragment.pixel_size, [8, 10]);
        assert_eq!(fragment.source, [0.0, 0.0, 7.5, 10.0]);
        assert_eq!(fragment.offset, [3, 0]);
    }
}
