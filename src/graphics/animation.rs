use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

const MAX_FRAME_BYTES: usize = 320 * 1024 * 1024;
const MAX_FRAME_COUNT: usize = 65_536;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FrameLoad {
    pub base_frame: u32,
    pub edit_frame: u32,
    pub x: u32,
    pub y: u32,
    pub gap: i32,
    pub replace: bool,
    pub background: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Control {
    pub state: u32,
    pub current_frame: u32,
    pub gap_frame: u32,
    pub gap: i32,
    pub loops: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Compose {
    pub source_frame: u32,
    pub destination_frame: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub destination_x: u32,
    pub destination_y: u32,
    pub width: u32,
    pub height: u32,
    pub replace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Stopped,
    Loading,
    Running,
}

#[derive(Clone)]
struct Overlay {
    base_id: Option<u64>,
    rgba: Arc<[u8]>,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    replace: bool,
    background: [u8; 4],
}

#[derive(Clone)]
struct Frame {
    id: u64,
    rgba: Arc<[u8]>,
    overlay: Option<Overlay>,
    gap: Duration,
}

pub(crate) struct Animation {
    width: u32,
    height: u32,
    frames: Vec<Frame>,
    next_id: u64,
    current: usize,
    state: State,
    max_loops: u32,
    completed_loops: u32,
    animation_duration: Duration,
    shown_at: Duration,
    revision: u64,
    storage_limit: usize,
}

impl Animation {
    pub fn new(
        width: u32,
        height: u32,
        root: Arc<[u8]>,
        now: Duration,
    ) -> Result<Self, &'static str> {
        if image_bytes(width, height)? != root.len() {
            return Err("EINVAL");
        }
        Ok(Self {
            width,
            height,
            frames: vec![Frame {
                id: 1,
                rgba: root,
                overlay: None,
                gap: Duration::ZERO,
            }],
            next_id: 2,
            current: 0,
            state: State::Stopped,
            max_loops: 0,
            completed_loops: 0,
            animation_duration: Duration::ZERO,
            shown_at: now,
            revision: 1,
            storage_limit: MAX_FRAME_BYTES,
        })
    }

    pub fn load(
        &mut self,
        load: FrameLoad,
        rgba: Arc<[u8]>,
        width: u32,
        height: u32,
        now: Duration,
    ) -> Result<bool, &'static str> {
        if width > self.width || height > self.height || image_bytes(width, height)? != rgba.len() {
            return Err("EINVAL");
        }
        let before = self.current_pixels();
        let previous_duration = self.animation_duration;
        let edit = load
            .edit_frame
            .checked_sub(1)
            .map(|index| index as usize)
            .filter(|index| *index < self.frames.len());
        if let Some(index) = edit {
            let mut pixels = self.frames[index].rgba.to_vec();
            overlay_pixels(
                &mut pixels,
                self.width,
                self.height,
                &rgba,
                width,
                height,
                load.x,
                load.y,
                load.replace,
            );
            let mut changed = self.frames.clone();
            changed[index].rgba = pixels.into();
            changed[index].overlay = None;
            if load.gap != 0 {
                changed[index].gap = gap(load.gap);
            }
            rebuild_dependents(&mut changed, index, self.width, self.height)?;
            check_total_bytes(&changed, self.storage_limit)?;
            self.animation_duration = previous_duration
                .saturating_sub(self.frames[index].gap)
                .saturating_add(changed[index].gap);
            self.frames = changed;
        } else {
            if self.frames.len() >= MAX_FRAME_COUNT {
                return Err("ENOSPC");
            }
            let base_id = if load.base_frame == 0 {
                None
            } else {
                Some(self.frame(load.base_frame).ok_or("EINVAL")?.id)
            };
            let background = load.background.to_be_bytes();
            let full = width == self.width
                && height == self.height
                && load.x == 0
                && load.y == 0
                && base_id.is_none()
                && (load.replace || background == [0; 4]);
            let overlay = (!full).then_some(Overlay {
                base_id,
                rgba: rgba.clone(),
                width,
                height,
                x: load.x,
                y: load.y,
                replace: load.replace,
                background,
            });
            let incoming_stored = self.frames.iter().any(|frame| {
                Arc::ptr_eq(&frame.rgba, &rgba)
                    || frame
                        .overlay
                        .as_ref()
                        .is_some_and(|overlay| Arc::ptr_eq(&overlay.rgba, &rgba))
            });
            let incoming_bytes = if incoming_stored { 0 } else { rgba.len() };
            let canvas_bytes = if overlay.is_some() {
                image_bytes(self.width, self.height)?
            } else {
                0
            };
            if self
                .total_bytes()
                .saturating_add(incoming_bytes)
                .saturating_add(canvas_bytes)
                > self.storage_limit
            {
                return Err("ENOSPC");
            }
            let pixels = match overlay.as_ref() {
                Some(overlay) => render_overlay(overlay, &self.frames, self.width, self.height)?,
                None => rgba,
            };
            let frame = Frame {
                id: self.next_id,
                rgba: pixels,
                overlay,
                gap: if load.gap == 0 {
                    Duration::from_millis(40)
                } else {
                    gap(load.gap)
                },
            };
            self.frames.push(frame);
            if let Err(error) = check_total_bytes(&self.frames, self.storage_limit) {
                self.frames.pop();
                return Err(error);
            }
            self.animation_duration =
                previous_duration.saturating_add(self.frames.last().unwrap().gap);
            self.next_id = self.next_id.wrapping_add(1);
        }
        Ok(self.mark_changed(before, now, false))
    }

    pub fn control(&mut self, control: Control, now: Duration) -> Result<bool, &'static str> {
        if control.state > 3 {
            return Err("EINVAL");
        }
        let before = self.current_pixels();
        if control.gap != 0
            && let Some(index) = self.frame_index(control.gap_frame)
        {
            self.animation_duration = self
                .animation_duration
                .saturating_sub(self.frames[index].gap)
                .saturating_add(gap(control.gap));
            self.frames[index].gap = gap(control.gap);
        }
        if let Some(index) = self.frame_index(control.current_frame)
            && index != self.current
        {
            self.current = index;
            self.shown_at = now;
        }
        if control.state != 0 {
            let previous = self.state;
            self.state = match control.state {
                1 => State::Stopped,
                2 => State::Loading,
                3 => State::Running,
                _ => unreachable!(),
            };
            self.completed_loops = 0;
            if previous == State::Stopped && self.state != State::Stopped {
                self.shown_at = now;
            }
        }
        if control.loops != 0 {
            self.max_loops = control.loops - 1;
        }
        Ok(self.mark_changed(before, now, false))
    }

    pub fn compose(&mut self, compose: Compose, now: Duration) -> Result<bool, &'static str> {
        let source = self.frame_index(compose.source_frame).ok_or("ENOENT")?;
        let destination = self
            .frame_index(compose.destination_frame)
            .ok_or("ENOENT")?;
        let width = if compose.width == 0 {
            self.width
        } else {
            compose.width
        };
        let height = if compose.height == 0 {
            self.height
        } else {
            compose.height
        };
        for (x, y) in [
            (compose.source_x, compose.source_y),
            (compose.destination_x, compose.destination_y),
        ] {
            if u64::from(x) + u64::from(width) > u64::from(self.width)
                || u64::from(y) + u64::from(height) > u64::from(self.height)
            {
                return Err("EINVAL");
            }
        }
        if source == destination
            && rectangles_overlap(
                compose.source_x,
                compose.source_y,
                compose.destination_x,
                compose.destination_y,
                width,
                height,
            )
        {
            return Err("EINVAL");
        }
        let before = self.current_pixels();
        let source_pixels = self.frames[source].rgba.clone();
        let mut pixels = self.frames[destination].rgba.to_vec();
        for row in 0..height {
            let source_offset = pixel_offset(self.width, compose.source_x, compose.source_y + row);
            let destination_offset = pixel_offset(
                self.width,
                compose.destination_x,
                compose.destination_y + row,
            );
            let length = width as usize * 4;
            blend_row(
                &mut pixels[destination_offset..destination_offset + length],
                &source_pixels[source_offset..source_offset + length],
                compose.replace,
            );
        }
        let mut changed = self.frames.clone();
        changed[destination].rgba = pixels.into();
        changed[destination].overlay = None;
        rebuild_dependents(&mut changed, destination, self.width, self.height)?;
        check_total_bytes(&changed, self.storage_limit)?;
        self.frames = changed;
        Ok(self.mark_changed(before, now, false))
    }

    pub fn delete_frame(
        &mut self,
        frame: u32,
        delete_last: bool,
        now: Duration,
    ) -> Result<bool, &'static str> {
        if self.frames.len() == 1 {
            return Ok(delete_last);
        }
        let index = frame.saturating_sub(1).min(self.frames.len() as u32 - 1) as usize;
        let before = self.current_pixels();
        let removed_id = self.frames[index].id;
        self.animation_duration = self
            .animation_duration
            .saturating_sub(self.frames[index].gap);
        for frame in &mut self.frames {
            if frame
                .overlay
                .as_ref()
                .is_some_and(|overlay| overlay.base_id == Some(removed_id))
            {
                frame.overlay = None;
            }
        }
        self.frames.remove(index);
        if self.current > index {
            self.current -= 1;
        } else if self.current == index {
            self.current = index.min(self.frames.len() - 1);
            self.shown_at = now;
        }
        self.mark_changed(before, now, true);
        Ok(false)
    }

    pub fn advance(&mut self, now: Duration) -> bool {
        let Some(deadline) = self.next_deadline() else {
            return false;
        };
        if now < deadline {
            return false;
        }
        let before = self.current_pixels();
        let previous = self.current;
        for _ in 0..self.frames.len() {
            let next = (self.current + 1) % self.frames.len();
            if next == 0 {
                if self.state == State::Loading {
                    self.current = previous;
                    return false;
                }
                self.completed_loops = self.completed_loops.saturating_add(1);
                if self.max_loops != 0 && self.completed_loops >= self.max_loops {
                    self.current = previous;
                    return false;
                }
            }
            self.current = next;
            if !self.frames[self.current].gap.is_zero() {
                break;
            }
        }
        self.shown_at = now;
        self.mark_changed(before, now, true)
    }

    pub fn next_deadline(&self) -> Option<Duration> {
        if self.state == State::Stopped
            || self.frames.len() < 2
            || (self.max_loops != 0 && self.completed_loops >= self.max_loops)
            || self.animation_duration.is_zero()
            || (self.state == State::Loading
                && self.frames[self.current + 1..]
                    .iter()
                    .all(|frame| frame.gap.is_zero()))
        {
            return None;
        }
        Some(self.shown_at.saturating_add(self.frames[self.current].gap))
    }

    pub fn current_pixels(&self) -> Arc<[u8]> {
        self.frames[self.current].rgba.clone()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn total_bytes(&self) -> usize {
        frame_bytes(&self.frames)
    }

    pub fn set_storage_limit(&mut self, limit: usize) {
        self.storage_limit = limit.min(MAX_FRAME_BYTES);
    }

    pub fn frame_count(&self) -> u32 {
        self.frames.len() as u32
    }

    #[cfg(test)]
    pub fn current_frame(&self) -> u32 {
        self.current as u32 + 1
    }

    fn frame_index(&self, number: u32) -> Option<usize> {
        number
            .checked_sub(1)
            .map(|index| index as usize)
            .filter(|index| *index < self.frames.len())
    }

    fn frame(&self, number: u32) -> Option<&Frame> {
        self.frame_index(number).map(|index| &self.frames[index])
    }

    fn mark_changed(&mut self, before: Arc<[u8]>, now: Duration, reset_time: bool) -> bool {
        let changed = !Arc::ptr_eq(&before, &self.frames[self.current].rgba);
        if changed {
            self.revision = self.revision.wrapping_add(1);
            if reset_time {
                self.shown_at = now;
            }
        }
        changed
    }
}

fn image_bytes(width: u32, height: u32) -> Result<usize, &'static str> {
    if width == 0 || height == 0 {
        return Err("EINVAL");
    }
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("ENOSPC")?;
    if bytes > MAX_FRAME_BYTES as u64 {
        return Err("ENOSPC");
    }
    usize::try_from(bytes).map_err(|_| "ENOSPC")
}

fn frame_bytes(frames: &[Frame]) -> usize {
    let mut seen = HashSet::new();
    let mut bytes = 0;
    for frame in frames {
        for pixels in
            std::iter::once(&frame.rgba).chain(frame.overlay.iter().map(|overlay| &overlay.rgba))
        {
            if seen.insert(pixels.as_ptr()) {
                bytes += pixels.len();
            }
        }
    }
    bytes
}

fn check_total_bytes(frames: &[Frame], limit: usize) -> Result<(), &'static str> {
    if frame_bytes(frames) > limit {
        Err("ENOSPC")
    } else {
        Ok(())
    }
}

fn gap(milliseconds: i32) -> Duration {
    Duration::from_millis(milliseconds.max(0) as u64)
}

fn render_overlay(
    overlay: &Overlay,
    frames: &[Frame],
    width: u32,
    height: u32,
) -> Result<Arc<[u8]>, &'static str> {
    let mut pixels = match overlay.base_id {
        Some(id) => frames
            .iter()
            .find(|frame| frame.id == id)
            .ok_or("ENOENT")?
            .rgba
            .to_vec(),
        None => {
            let mut pixels = vec![0; image_bytes(width, height)?];
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(&overlay.background);
            }
            pixels
        }
    };
    overlay_pixels(
        &mut pixels,
        width,
        height,
        &overlay.rgba,
        overlay.width,
        overlay.height,
        overlay.x,
        overlay.y,
        overlay.replace,
    );
    Ok(pixels.into())
}

fn rebuild_dependents(
    frames: &mut [Frame],
    index: usize,
    width: u32,
    height: u32,
) -> Result<(), &'static str> {
    let mut changed = HashSet::from([frames[index].id]);
    for current in index + 1..frames.len() {
        if let Some(overlay) = frames[current].overlay.as_ref()
            && overlay.base_id.is_some_and(|id| changed.contains(&id))
        {
            let pixels = render_overlay(overlay, frames, width, height)?;
            frames[current].rgba = pixels;
            changed.insert(frames[current].id);
        }
    }
    Ok(())
}

fn overlay_pixels(
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    pixels: &[u8],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    replace: bool,
) {
    let clipped_width = width.min(canvas_width.saturating_sub(x));
    let clipped_height = height.min(canvas_height.saturating_sub(y));
    if clipped_width == 0 || clipped_height == 0 {
        return;
    }
    for row in 0..clipped_height {
        let destination = pixel_offset(canvas_width, x, y + row);
        let source = pixel_offset(width, 0, row);
        let length = clipped_width as usize * 4;
        blend_row(
            &mut canvas[destination..destination + length],
            &pixels[source..source + length],
            replace,
        );
    }
}

fn pixel_offset(width: u32, x: u32, y: u32) -> usize {
    ((u64::from(y) * u64::from(width) + u64::from(x)) * 4) as usize
}

fn blend_row(destination: &mut [u8], source: &[u8], replace: bool) {
    if replace {
        destination.copy_from_slice(source);
        return;
    }
    for (under, over) in destination.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
        let alpha = u32::from(over[3]);
        if alpha == 255 {
            under.copy_from_slice(over);
        } else if alpha != 0 {
            let under_alpha = u32::from(under[3]);
            let inverse = 255 - alpha;
            let combined = alpha * 255 + under_alpha * inverse;
            for channel in 0..3 {
                let numerator = u32::from(over[channel]) * alpha * 255
                    + u32::from(under[channel]) * under_alpha * inverse;
                under[channel] = (numerator as f32 / combined as f32).round_ties_even() as u8;
            }
            under[3] = ((combined + 127) / 255) as u8;
        }
    }
}

fn rectangles_overlap(
    source_x: u32,
    source_y: u32,
    destination_x: u32,
    destination_y: u32,
    width: u32,
    height: u32,
) -> bool {
    u64::from(source_x.max(destination_x))
        < u64::from(source_x.min(destination_x)) + u64::from(width)
        && u64::from(source_y.max(destination_y))
            < u64::from(source_y.min(destination_y)) + u64::from(height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn milliseconds(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn solid(width: u32, height: u32, pixel: [u8; 4]) -> Arc<[u8]> {
        let mut bytes = Vec::new();
        for _ in 0..width * height {
            bytes.extend_from_slice(&pixel);
        }
        bytes.into()
    }

    fn image() -> Animation {
        Animation::new(2, 2, solid(2, 2, [255, 0, 0, 255]), Duration::ZERO).unwrap()
    }

    fn append(animation: &mut Animation, pixel: [u8; 4], delay: i32) {
        animation
            .load(
                FrameLoad {
                    gap: delay,
                    ..Default::default()
                },
                solid(2, 2, pixel),
                2,
                2,
                Duration::ZERO,
            )
            .unwrap();
    }

    #[test]
    fn terminal_animation_skips_gapless_root_and_holds_last_frame_after_one_loop() {
        let mut animation = image();
        append(&mut animation, [0, 255, 0, 255], 25);
        append(&mut animation, [0, 0, 255, 255], 40);
        animation
            .control(
                Control {
                    state: 3,
                    loops: 2,
                    ..Default::default()
                },
                milliseconds(100),
            )
            .unwrap();
        assert_eq!(animation.next_deadline(), Some(milliseconds(100)));
        assert!(animation.advance(milliseconds(100)));
        assert_eq!(animation.current_frame(), 2);
        assert!(!animation.advance(milliseconds(124)));
        assert!(animation.advance(milliseconds(125)));
        assert_eq!(animation.current_frame(), 3);
        assert!(!animation.advance(milliseconds(165)));
        assert_eq!(animation.current_frame(), 3);
        assert_eq!(animation.next_deadline(), None);
    }

    #[test]
    fn loading_waits_for_new_frames_without_busy_deadlines() {
        let mut animation = image();
        animation
            .control(
                Control {
                    state: 2,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(animation.next_deadline(), None);
        append(&mut animation, [0, 255, 0, 255], 40);
        assert!(animation.advance(milliseconds(50)));
        assert_eq!(animation.current_frame(), 2);
        assert_eq!(animation.next_deadline(), None);
        append(&mut animation, [0, 0, 255, 255], 40);
        assert_eq!(animation.next_deadline(), Some(milliseconds(90)));
        assert!(animation.advance(milliseconds(90)));
        assert_eq!(animation.current_frame(), 3);
        assert_eq!(animation.next_deadline(), None);
    }

    #[test]
    fn stopping_resets_loop_counter_and_restarting_resets_timebase() {
        let mut animation = image();
        append(&mut animation, [0, 255, 0, 255], 40);
        animation
            .control(
                Control {
                    state: 3,
                    loops: 2,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert!(animation.advance(Duration::ZERO));
        assert!(!animation.advance(milliseconds(40)));
        animation
            .control(
                Control {
                    state: 1,
                    current_frame: 1,
                    ..Default::default()
                },
                milliseconds(100),
            )
            .unwrap();
        animation
            .control(
                Control {
                    state: 3,
                    ..Default::default()
                },
                milliseconds(500),
            )
            .unwrap();
        assert_eq!(animation.next_deadline(), Some(milliseconds(500)));
        assert!(animation.advance(milliseconds(500)));
    }

    #[test]
    fn all_gapless_frames_have_no_animation_deadline() {
        let mut animation = image();
        append(&mut animation, [0, 255, 0, 255], -1);
        animation
            .control(
                Control {
                    state: 3,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(animation.next_deadline(), None);
        assert!(!animation.advance(Duration::MAX));
    }

    #[test]
    fn trailing_gapless_frames_are_not_displayed_when_loading_waits() {
        let mut animation = image();
        append(&mut animation, [0, 255, 0, 255], 40);
        append(&mut animation, [0, 0, 255, 255], -1);
        animation
            .control(
                Control {
                    state: 2,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert!(animation.advance(Duration::ZERO));
        let visible = animation.current_pixels();
        assert!(!animation.advance(milliseconds(40)));
        assert!(Arc::ptr_eq(&visible, &animation.current_pixels()));
        assert_eq!(animation.current_frame(), 2);
        assert_eq!(animation.next_deadline(), None);
    }

    #[test]
    fn patch_blending_and_replacement_use_straight_rgba() {
        let mut animation = image();
        animation
            .load(
                FrameLoad {
                    base_frame: 1,
                    x: 1,
                    ..Default::default()
                },
                solid(1, 1, [0, 0, 255, 128]),
                1,
                1,
                Duration::ZERO,
            )
            .unwrap();
        animation
            .control(
                Control {
                    current_frame: 2,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(&animation.current_pixels()[4..8], &[127, 0, 128, 255]);
        animation
            .load(
                FrameLoad {
                    edit_frame: 2,
                    x: 1,
                    replace: true,
                    ..Default::default()
                },
                solid(1, 1, [11, 12, 13, 0]),
                1,
                1,
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(&animation.current_pixels()[4..8], &[11, 12, 13, 0]);
    }

    #[test]
    fn transparent_edits_do_not_erase_without_replacement_and_zero_gap_is_ignored() {
        let mut animation = image();
        append(&mut animation, [0, 255, 0, 255], 77);
        animation
            .control(
                Control {
                    current_frame: 2,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        animation
            .load(
                FrameLoad {
                    edit_frame: 2,
                    ..Default::default()
                },
                solid(2, 2, [0, 0, 0, 0]),
                2,
                2,
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(
            animation.current_pixels().as_ref(),
            solid(2, 2, [0, 255, 0, 255]).as_ref()
        );
        assert_eq!(animation.frames[1].gap, milliseconds(77));
    }

    #[test]
    fn base_edits_update_dependents_and_deletion_promotes_them() {
        let mut animation = image();
        animation
            .load(
                FrameLoad {
                    base_frame: 1,
                    ..Default::default()
                },
                solid(1, 1, [0, 255, 0, 255]),
                1,
                1,
                Duration::ZERO,
            )
            .unwrap();
        animation
            .load(
                FrameLoad {
                    base_frame: 2,
                    x: 1,
                    y: 1,
                    ..Default::default()
                },
                solid(1, 1, [0, 0, 255, 255]),
                1,
                1,
                Duration::ZERO,
            )
            .unwrap();
        animation
            .load(
                FrameLoad {
                    edit_frame: 1,
                    replace: true,
                    ..Default::default()
                },
                solid(2, 2, [30, 40, 50, 255]),
                2,
                2,
                Duration::ZERO,
            )
            .unwrap();
        animation
            .control(
                Control {
                    current_frame: 3,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(
            animation.current_pixels().as_ref(),
            &[
                0, 255, 0, 255, 30, 40, 50, 255, 30, 40, 50, 255, 0, 0, 255, 255
            ]
        );
        let preserved = animation.current_pixels();
        assert!(!animation.delete_frame(1, false, Duration::ZERO).unwrap());
        assert_eq!(animation.current_frame(), 2);
        assert_eq!(animation.current_pixels(), preserved);
        animation
            .load(
                FrameLoad {
                    edit_frame: 1,
                    replace: true,
                    ..Default::default()
                },
                solid(2, 2, [60, 70, 80, 255]),
                2,
                2,
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(&animation.current_pixels()[4..8], &[60, 70, 80, 255]);
    }

    #[test]
    fn deletion_clamps_frame_number_promotes_root_and_retains_final_frame() {
        let mut animation = image();
        append(&mut animation, [0, 255, 0, 255], 40);
        append(&mut animation, [0, 0, 255, 255], 40);
        animation
            .control(
                Control {
                    current_frame: 3,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert!(
            !animation
                .delete_frame(u32::MAX, false, Duration::ZERO)
                .unwrap()
        );
        assert_eq!(animation.current_frame(), 2);
        assert!(!animation.delete_frame(0, false, Duration::ZERO).unwrap());
        assert_eq!(animation.current_frame(), 1);
        assert_eq!(
            animation.current_pixels().as_ref(),
            solid(2, 2, [0, 255, 0, 255]).as_ref()
        );
        assert!(!animation.delete_frame(1, false, Duration::ZERO).unwrap());
        assert!(animation.delete_frame(1, true, Duration::ZERO).unwrap());
    }

    #[test]
    fn composition_rejects_missing_overflowing_and_overlapping_rectangles() {
        let mut animation = image();
        append(&mut animation, [0, 255, 0, 255], 40);
        assert_eq!(
            animation.compose(
                Compose {
                    source_frame: 7,
                    destination_frame: 1,
                    ..Default::default()
                },
                Duration::ZERO
            ),
            Err("ENOENT")
        );
        assert_eq!(
            animation.compose(
                Compose {
                    source_frame: 1,
                    destination_frame: 2,
                    source_x: u32::MAX,
                    ..Default::default()
                },
                Duration::ZERO
            ),
            Err("EINVAL")
        );
        assert_eq!(
            animation.compose(
                Compose {
                    source_frame: 1,
                    destination_frame: 1,
                    ..Default::default()
                },
                Duration::ZERO
            ),
            Err("EINVAL")
        );
        animation
            .compose(
                Compose {
                    source_frame: 1,
                    destination_frame: 1,
                    source_x: 1,
                    destination_x: 0,
                    width: 1,
                    height: 2,
                    replace: true,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
    }

    #[test]
    fn composition_materializes_destination_and_preserves_independent_pixels() {
        let mut animation = image();
        animation
            .load(
                FrameLoad {
                    base_frame: 1,
                    ..Default::default()
                },
                solid(1, 1, [0, 255, 0, 255]),
                1,
                1,
                Duration::ZERO,
            )
            .unwrap();
        animation
            .compose(
                Compose {
                    source_frame: 1,
                    destination_frame: 2,
                    source_x: 1,
                    source_y: 1,
                    destination_x: 1,
                    destination_y: 0,
                    width: 1,
                    height: 1,
                    replace: true,
                },
                Duration::ZERO,
            )
            .unwrap();
        animation
            .load(
                FrameLoad {
                    edit_frame: 1,
                    replace: true,
                    ..Default::default()
                },
                solid(2, 2, [0, 0, 255, 255]),
                2,
                2,
                Duration::ZERO,
            )
            .unwrap();
        animation
            .control(
                Control {
                    current_frame: 2,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(&animation.current_pixels()[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn canvas_color_and_partial_overflow_match_kitty_clipping() {
        let mut animation = image();
        animation
            .load(
                FrameLoad {
                    x: 1,
                    y: 1,
                    background: 0x12345678,
                    replace: true,
                    ..Default::default()
                },
                solid(2, 2, [0, 255, 0, 255]),
                2,
                2,
                Duration::ZERO,
            )
            .unwrap();
        animation
            .control(
                Control {
                    current_frame: 2,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(&animation.current_pixels()[0..4], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(&animation.current_pixels()[12..16], &[0, 255, 0, 255]);
        assert_eq!(
            animation.load(
                FrameLoad::default(),
                solid(3, 1, [0; 4]),
                3,
                1,
                Duration::ZERO
            ),
            Err("EINVAL")
        );
    }

    #[test]
    fn malformed_pixel_lengths_and_absurd_dimensions_are_bounded() {
        assert!(matches!(
            Animation::new(0, 1, Arc::from([]), Duration::ZERO),
            Err("EINVAL")
        ));
        assert!(matches!(
            Animation::new(u32::MAX, u32::MAX, Arc::from([]), Duration::ZERO),
            Err("ENOSPC")
        ));
        let mut animation = image();
        assert_eq!(
            animation.load(
                FrameLoad::default(),
                Arc::from([0u8; 3]),
                1,
                1,
                Duration::ZERO
            ),
            Err("EINVAL")
        );
        assert_eq!(animation.total_bytes(), 16);
        assert_eq!(animation.frame_count(), 1);
    }

    #[test]
    fn unchanged_frame_storage_does_not_allocate_a_new_visible_revision() {
        let mut animation = image();
        let pixels = animation.current_pixels();
        let revision = animation.revision();
        animation
            .load(FrameLoad::default(), pixels.clone(), 2, 2, Duration::ZERO)
            .unwrap();
        assert_eq!(animation.total_bytes(), 16);
        assert_eq!(animation.revision(), revision);
        animation
            .control(
                Control {
                    state: 3,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert!(!animation.advance(milliseconds(40)));
        assert_eq!(animation.next_deadline(), Some(milliseconds(80)));
        assert_eq!(animation.revision(), revision);
    }

    #[test]
    fn frame_metadata_is_bounded_even_when_payload_storage_is_shared() {
        let mut animation = image();
        let frame = animation.frames[0].clone();
        animation.frames.resize(MAX_FRAME_COUNT, frame);
        assert_eq!(
            animation.load(
                FrameLoad::default(),
                animation.current_pixels(),
                2,
                2,
                Duration::ZERO
            ),
            Err("ENOSPC")
        );
        assert_eq!(animation.frames.len(), MAX_FRAME_COUNT);
    }

    #[test]
    fn straight_alpha_uses_kittys_round_to_nearest_even() {
        let mut actual = [0, 0, 0, 2];
        blend_row(&mut actual, &[1, 1, 1, 2], false);
        assert_eq!(actual, [1, 1, 1, 4]);
        let mut actual = [0, 0, 0, 255];
        blend_row(&mut actual, &[255, 127, 19, 128], false);
        assert_eq!(actual, [128, 64, 10, 255]);
    }

    #[test]
    fn caller_storage_limit_accounts_for_expanded_canvases_before_loading() {
        let mut animation = image();
        animation.set_storage_limit(35);
        let pixels = animation.current_pixels();
        assert_eq!(
            animation.load(
                FrameLoad {
                    base_frame: 1,
                    ..Default::default()
                },
                solid(1, 1, [0, 255, 0, 255]),
                1,
                1,
                Duration::ZERO
            ),
            Err("ENOSPC")
        );
        assert_eq!(animation.frame_count(), 1);
        assert_eq!(animation.total_bytes(), 16);
        assert!(Arc::ptr_eq(&pixels, &animation.current_pixels()));
        animation.set_storage_limit(36);
        animation
            .load(
                FrameLoad {
                    base_frame: 1,
                    ..Default::default()
                },
                solid(1, 1, [0, 255, 0, 255]),
                1,
                1,
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(animation.total_bytes(), 36);
    }

    #[test]
    fn editing_and_composition_respect_caller_limit_atomically() {
        let mut animation = image();
        animation
            .load(
                FrameLoad::default(),
                animation.current_pixels(),
                2,
                2,
                Duration::ZERO,
            )
            .unwrap();
        animation.set_storage_limit(16);
        let root = animation.current_pixels();
        assert_eq!(
            animation.load(
                FrameLoad {
                    edit_frame: 1,
                    replace: true,
                    ..Default::default()
                },
                solid(1, 1, [0, 255, 0, 255]),
                1,
                1,
                Duration::ZERO
            ),
            Err("ENOSPC")
        );
        assert!(Arc::ptr_eq(&root, &animation.current_pixels()));
        assert_eq!(
            animation.compose(
                Compose {
                    source_frame: 1,
                    destination_frame: 2,
                    ..Default::default()
                },
                Duration::ZERO
            ),
            Err("ENOSPC")
        );
        assert!(Arc::ptr_eq(&root, &animation.current_pixels()));
        assert_eq!(animation.total_bytes(), 16);
        assert_eq!(animation.revision(), 1);
        animation.set_storage_limit(32);
        animation
            .compose(
                Compose {
                    source_frame: 1,
                    destination_frame: 2,
                    ..Default::default()
                },
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(animation.total_bytes(), 32);
    }
}
