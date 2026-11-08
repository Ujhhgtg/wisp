const SHORT_ANIMATION_LENGTH: f32 = 0.04;
const STANDARD_CORNERS: [[f32; 2]; 4] = [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

#[derive(Clone, Copy, Debug)]
pub struct CursorAnimationOptions {
    pub smooth: bool,
    pub trail: bool,
    pub duration_seconds: f32,
    pub cell_percentage: f32,
}

impl Default for CursorAnimationOptions {
    fn default() -> Self {
        Self {
            smooth: true,
            trail: true,
            duration_seconds: 0.150,
            cell_percentage: 1.0 / 8.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorFrame {
    pub corners: [[f32; 2]; 4],
    pub animating: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Spring {
    position: f32,
    velocity: f32,
}

impl Spring {
    fn update(&mut self, dt: f32, animation_length: f32) -> bool {
        if animation_length <= dt {
            self.reset();
            return false;
        }
        if self.position == 0.0 {
            return false;
        }

        let omega = 4.0 / animation_length;
        let initial_position = self.position;
        let initial_velocity = self.position * omega + self.velocity;
        let decay = (-omega * dt).exp();

        self.position = (initial_position + initial_velocity * dt) * decay;
        self.velocity =
            decay * (-initial_position * omega - initial_velocity * dt * omega + initial_velocity);

        if self.position.abs() < 0.01 {
            self.reset();
            false
        } else {
            true
        }
    }

    fn reset(&mut self) {
        self.position = 0.0;
        self.velocity = 0.0;
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Corner {
    current_position: [f32; 2],
    previous_destination: [f32; 2],
    relative_position: [f32; 2],
    horizontal: Spring,
    vertical: Spring,
    animation_length: f32,
}

impl Corner {
    fn destination(&self, center: [f32; 2], dimensions: [f32; 2]) -> [f32; 2] {
        [
            center[0] + self.relative_position[0] * dimensions[0],
            center[1] + self.relative_position[1] * dimensions[1],
        ]
    }

    fn direction_alignment(&self, center: [f32; 2], dimensions: [f32; 2]) -> f32 {
        let destination = self.destination(center, dimensions);
        let corner_direction = normalize(self.relative_position);
        let travel_direction = normalize([
            destination[0] - self.previous_destination[0],
            destination[1] - self.previous_destination[1],
        ]);
        corner_direction[0] * travel_direction[0] + corner_direction[1] * travel_direction[1]
    }

    fn jump(
        &mut self,
        center: [f32; 2],
        dimensions: [f32; 2],
        alignment: f32,
        options: CursorAnimationOptions,
    ) {
        let destination = self.destination(center, dimensions);
        let horizontal_jump = (destination[0] - self.previous_destination[0]) / dimensions[0];
        let vertical_jump = (destination[1] - self.previous_destination[1]) / dimensions[1];
        self.animation_length = if horizontal_jump.abs() <= 2.001 && vertical_jump.abs() <= 0.001 {
            options.duration_seconds.min(SHORT_ANIMATION_LENGTH)
        } else {
            let trailing = options.duration_seconds;
            let leading = if options.trail { 0.0 } else { trailing };
            trailing + (leading - trailing) * alignment
        };
    }

    fn update(&mut self, center: [f32; 2], dimensions: [f32; 2], dt: f32, immediate: bool) -> bool {
        let destination = self.destination(center, dimensions);
        if destination != self.previous_destination {
            self.horizontal.position = destination[0] - self.current_position[0];
            self.vertical.position = destination[1] - self.current_position[1];
            self.previous_destination = destination;
        }

        if immediate {
            self.horizontal.reset();
            self.vertical.reset();
            self.current_position = destination;
            return false;
        }

        let mut animating = self.horizontal.update(dt, self.animation_length);
        animating |= self.vertical.update(dt, self.animation_length);
        self.current_position = [
            destination[0] - self.horizontal.position,
            destination[1] - self.vertical.position,
        ];
        animating
    }
}

#[derive(Clone, Debug, Default)]
pub struct CursorAnimation {
    corners: [Corner; 4],
    previous_cell_rect: Option<[f32; 4]>,
}

impl CursorAnimation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hide(&mut self) {
        *self = Self::new();
    }

    pub fn update(
        &mut self,
        cell_rect: [f32; 4],
        shape: CursorShape,
        dt: f32,
        mut options: CursorAnimationOptions,
    ) -> CursorFrame {
        if cell_rect.iter().any(|component| !component.is_finite())
            || cell_rect[2] <= 0.0
            || cell_rect[3] <= 0.0
        {
            self.hide();
            return CursorFrame {
                corners: [[0.0; 2]; 4],
                animating: false,
            };
        }
        options.duration_seconds = finite_or(options.duration_seconds, 0.150).max(0.0);
        options.cell_percentage = finite_or(options.cell_percentage, 1.0 / 8.0).clamp(0.0, 1.0);
        let dt = finite_or(dt, 0.0).max(0.0);
        let dimensions = [cell_rect[2], cell_rect[3]];
        let center = [
            cell_rect[0] + dimensions[0] * 0.5,
            cell_rect[1] + dimensions[1] * 0.5,
        ];
        let immediate = !options.smooth
            || self
                .previous_cell_rect
                .is_none_or(|previous| previous[2] != cell_rect[2] || previous[3] != cell_rect[3]);

        for (corner, relative_position) in self.corners.iter_mut().zip(STANDARD_CORNERS) {
            corner.relative_position = match shape {
                CursorShape::Block => relative_position,
                CursorShape::Beam => [
                    (relative_position[0] + 0.5) * options.cell_percentage - 0.5,
                    relative_position[1],
                ],
                CursorShape::Underline => [
                    relative_position[0],
                    -((-relative_position[1] + 0.5) * options.cell_percentage - 0.5),
                ],
            };
        }

        let jumped = self.previous_cell_rect.is_some_and(|previous| {
            (cell_rect[0] - previous[0]).abs() > 0.01 || (cell_rect[1] - previous[1]).abs() > 0.01
        });
        if jumped && !immediate {
            let alignments = self
                .corners
                .map(|corner| corner.direction_alignment(center, dimensions));
            let minimum = alignments.iter().copied().fold(f32::INFINITY, f32::min);
            let maximum = alignments.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            for (corner, alignment) in self.corners.iter_mut().zip(alignments) {
                let alignment = (alignment - minimum) / (maximum - minimum);
                let alignment = if alignment.is_finite() {
                    alignment.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                corner.jump(center, dimensions, alignment, options);
            }
        }

        let mut animating = false;
        for corner in &mut self.corners {
            animating |= corner.update(center, dimensions, dt, immediate);
        }
        self.previous_cell_rect = Some(cell_rect);
        let fractional_origin = [cell_rect[0].fract(), cell_rect[1].fract()];
        CursorFrame {
            corners: self.corners.map(|corner| {
                [
                    (corner.current_position[0] - fractional_origin[0]).round()
                        + fractional_origin[0],
                    (corner.current_position[1] - fractional_origin[1]).round()
                        + fractional_origin[1],
                ]
            }),
            animating,
        }
    }
}

fn normalize(vector: [f32; 2]) -> [f32; 2] {
    let length = vector[0].hypot(vector[1]);
    [vector[0] / length, vector[1] / length]
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_snaps_to_the_requested_cursor_cell() {
        let mut animation = CursorAnimation::new();
        let frame = animation.update(
            [120.0, 80.0, 8.0, 16.0],
            CursorShape::Block,
            1.0 / 60.0,
            CursorAnimationOptions::default(),
        );
        assert_eq!(
            frame.corners,
            [[120.0, 80.0], [128.0, 80.0], [128.0, 96.0], [120.0, 96.0]]
        );
        assert!(!frame.animating);
    }

    #[test]
    fn neovide_default_long_jump_snaps_front_and_stretches_one_quad() {
        let mut animation = CursorAnimation::new();
        animation.update(
            [0.0, 0.0, 8.0, 16.0],
            CursorShape::Block,
            0.0,
            CursorAnimationOptions::default(),
        );
        let frame = animation.update(
            [80.0, 0.0, 8.0, 16.0],
            CursorShape::Block,
            1.0 / 60.0,
            CursorAnimationOptions::default(),
        );
        assert_eq!(
            frame.corners,
            [[6.0, 0.0], [88.0, 0.0], [88.0, 16.0], [6.0, 16.0]]
        );
        assert!((animation.corners[0].current_position[0] - 5.908_043).abs() < 0.000_1);
        assert!(frame.animating);
    }

    #[test]
    fn neovide_short_typing_jump_uses_forty_milliseconds_without_stretch() {
        let mut animation = CursorAnimation::new();
        animation.update(
            [0.0, 0.0, 8.0, 16.0],
            CursorShape::Block,
            0.0,
            CursorAnimationOptions::default(),
        );
        let frame = animation.update(
            [8.0, 0.0, 8.0, 16.0],
            CursorShape::Block,
            1.0 / 60.0,
            CursorAnimationOptions::default(),
        );
        assert_eq!(
            frame.corners,
            [[4.0, 0.0], [12.0, 0.0], [12.0, 16.0], [4.0, 16.0]]
        );
        assert!((animation.corners[0].current_position[0] - 3.970_654).abs() < 0.000_1);
        assert!(frame.animating);
    }

    #[test]
    fn analytical_spring_is_independent_of_frame_splitting() {
        let mut whole_frame = Spring {
            position: 80.0,
            velocity: 0.0,
        };
        let mut half_frames = whole_frame;
        whole_frame.update(1.0 / 60.0, 0.150);
        half_frames.update(1.0 / 120.0, 0.150);
        half_frames.update(1.0 / 120.0, 0.150);
        assert!((whole_frame.position - half_frames.position).abs() < 0.000_1);
        assert!((whole_frame.velocity - half_frames.velocity).abs() < 0.001);
    }

    #[test]
    fn geometry_change_and_reappearing_cursor_snap() {
        let mut animation = CursorAnimation::new();
        animation.update(
            [0.0, 0.0, 8.0, 16.0],
            CursorShape::Beam,
            0.0,
            CursorAnimationOptions::default(),
        );
        animation.update(
            [80.0, 0.0, 8.0, 16.0],
            CursorShape::Beam,
            1.0 / 60.0,
            CursorAnimationOptions::default(),
        );
        let resized = animation.update(
            [160.0, 32.0, 16.0, 32.0],
            CursorShape::Beam,
            1.0 / 60.0,
            CursorAnimationOptions::default(),
        );
        assert_eq!(
            resized.corners,
            [[160.0, 32.0], [162.0, 32.0], [162.0, 64.0], [160.0, 64.0]]
        );
        assert!(!resized.animating);
        animation.hide();
        let shown = animation.update(
            [240.0, 96.0, 16.0, 32.0],
            CursorShape::Beam,
            1.0 / 60.0,
            CursorAnimationOptions::default(),
        );
        assert_eq!(
            shown.corners,
            [[240.0, 96.0], [242.0, 96.0], [242.0, 128.0], [240.0, 128.0]]
        );
        assert!(!shown.animating);
    }

    #[test]
    fn disabling_trail_moves_a_rigid_cursor() {
        let mut animation = CursorAnimation::new();
        let options = CursorAnimationOptions {
            trail: false,
            ..Default::default()
        };
        animation.update([0.0, 0.0, 8.0, 16.0], CursorShape::Block, 0.0, options);
        let frame = animation.update(
            [80.0, 0.0, 8.0, 16.0],
            CursorShape::Block,
            1.0 / 60.0,
            options,
        );
        assert_eq!(frame.corners[1][0] - frame.corners[0][0], 8.0);
        assert_eq!(frame.corners[2][0] - frame.corners[3][0], 8.0);
    }

    #[test]
    fn disabling_smooth_movement_snaps_all_corners() {
        let mut animation = CursorAnimation::new();
        let options = CursorAnimationOptions {
            smooth: false,
            ..Default::default()
        };
        animation.update([0.0, 0.0, 8.0, 16.0], CursorShape::Block, 0.0, options);
        let frame = animation.update(
            [80.0, 48.0, 8.0, 16.0],
            CursorShape::Block,
            1.0 / 60.0,
            options,
        );
        assert_eq!(
            frame.corners,
            [[80.0, 48.0], [88.0, 48.0], [88.0, 64.0], [80.0, 64.0]]
        );
        assert!(!frame.animating);
    }

    #[test]
    fn underline_matches_neovide_shape_transform() {
        let mut animation = CursorAnimation::new();
        let frame = animation.update(
            [8.0, 16.0, 8.0, 16.0],
            CursorShape::Underline,
            0.0,
            CursorAnimationOptions::default(),
        );
        assert_eq!(
            frame.corners,
            [[8.0, 30.0], [16.0, 30.0], [16.0, 32.0], [8.0, 32.0]]
        );
    }

    #[test]
    fn settles_exactly_at_destination() {
        let mut animation = CursorAnimation::new();
        animation.update(
            [0.0, 0.0, 8.0, 16.0],
            CursorShape::Block,
            0.0,
            CursorAnimationOptions::default(),
        );
        let mut frame = animation.update(
            [80.0, 48.0, 8.0, 16.0],
            CursorShape::Block,
            1.0 / 60.0,
            CursorAnimationOptions::default(),
        );
        for _ in 0..120 {
            frame = animation.update(
                [80.0, 48.0, 8.0, 16.0],
                CursorShape::Block,
                1.0 / 60.0,
                CursorAnimationOptions::default(),
            );
        }
        assert_eq!(
            frame.corners,
            [[80.0, 48.0], [88.0, 48.0], [88.0, 64.0], [80.0, 64.0]]
        );
        assert!(!frame.animating);
    }

    #[test]
    fn fractional_cell_origin_is_preserved_when_snapping() {
        let mut animation = CursorAnimation::new();
        let frame = animation.update(
            [8.25, 16.5, 8.0, 16.0],
            CursorShape::Block,
            0.0,
            CursorAnimationOptions::default(),
        );
        assert_eq!(
            frame.corners,
            [[8.25, 16.5], [16.25, 16.5], [16.25, 32.5], [8.25, 32.5]]
        );
    }
}
