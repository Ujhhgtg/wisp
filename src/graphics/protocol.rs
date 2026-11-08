use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Instant;

use base64::Engine;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use libghostty_vt::kitty::graphics::Image;
use libghostty_vt::screen::Screen;
use libghostty_vt::style::PaletteIndex;
use libghostty_vt::{Error, Terminal};

use super::animation::{Animation, Compose, Control, FrameLoad};
use super::{ImageFormat, ImagePixels, MAX_DECODED_BYTES, normalize_pixels};
use crate::config::Config;

const MAX_SEQUENCE_BYTES: usize = 96 * 1024 * 1024;
const MAX_ANIMATION_BYTES: usize = 320 * 1024 * 1024;
const PAYLOAD_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

pub(super) struct Protocol {
    filter: Filter,
    native_buffer: Vec<u8>,
    pending: Option<Pending>,
    animations: HashMap<u64, AnimatedImage>,
    indexed: HashMap<u64, IndexedImage>,
    started: Instant,
    next_id: u32,
}

struct AnimatedImage {
    screen: Screen,
    image_id: u32,
    animation: Animation,
    revision: u64,
    pixels: Arc<ImagePixels>,
    cache: HashMap<usize, (Weak<[u8]>, Arc<ImagePixels>)>,
    visible: bool,
}

struct IndexedImage {
    screen: Screen,
    image_id: u32,
    width: u32,
    height: u32,
    channels: usize,
    indices: Vec<u8>,
    palette: Vec<[u8; 4]>,
    pixels: Arc<ImagePixels>,
}

struct Pending {
    fields: Fields,
    payload: Vec<u8>,
}

#[derive(Clone, Default)]
struct Fields(BTreeMap<u8, String>);

impl Fields {
    fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let text = std::str::from_utf8(bytes).map_err(|_| "EINVAL")?;
        let mut result = Self::default();
        for field in text.split(',').filter(|field| !field.is_empty()) {
            let (key, value) = field.split_once('=').ok_or("EINVAL")?;
            if key.len() != 1 || value.is_empty() || !value.is_ascii() {
                return Err("EINVAL");
            }
            result.0.insert(key.as_bytes()[0], value.to_owned());
        }
        Ok(result)
    }

    fn character(&self, key: u8, default: u8) -> u8 {
        self.0
            .get(&key)
            .and_then(|value| (value.len() == 1).then_some(value.as_bytes()[0]))
            .unwrap_or(default)
    }

    fn number(&self, key: u8) -> Result<u32, &'static str> {
        self.0
            .get(&key)
            .map_or(Ok(0), |value| value.parse().map_err(|_| "EINVAL"))
    }

    fn signed(&self, key: u8) -> Result<i32, &'static str> {
        self.0
            .get(&key)
            .map_or(Ok(0), |value| value.parse().map_err(|_| "EINVAL"))
    }

    fn set(&mut self, key: u8, value: impl ToString) {
        self.0.insert(key, value.to_string());
    }

    fn packet(&self, payload: &[u8]) -> Vec<u8> {
        let mut packet = b"\x1b_G".to_vec();
        for (index, (key, value)) in self.0.iter().enumerate() {
            if index != 0 {
                packet.push(b',');
            }
            packet.push(*key);
            packet.push(b'=');
            packet.extend_from_slice(value.as_bytes());
        }
        packet.push(b';');
        packet.extend_from_slice(payload);
        packet.extend_from_slice(b"\x1b\\");
        packet
    }
}

impl Protocol {
    pub fn new() -> Self {
        Self {
            filter: Filter::default(),
            native_buffer: Vec::new(),
            pending: None,
            animations: HashMap::new(),
            indexed: HashMap::new(),
            started: Instant::now(),
            next_id: 1,
        }
    }

    pub fn write(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        data: &[u8],
        reply: &mut dyn FnMut(&[u8]),
    ) {
        if !config.protocols.kitty_graphics {
            self.pending = None;
            self.animations.clear();
            self.indexed.clear();
            terminal.vt_write(data);
            return;
        }
        if matches!(self.filter.state, FilterState::Ground) && !needs_filter(data) {
            terminal.vt_write(data);
            return;
        }
        let mut filter = std::mem::take(&mut self.filter);
        let mut native = std::mem::take(&mut self.native_buffer);
        filter.write(data, |graphics, bytes| {
            if graphics {
                if !native.is_empty() {
                    terminal.vt_write(&native);
                    native.clear();
                }
                self.command(terminal, config, bytes, reply);
            } else {
                if bytes == b"\x1bc" {
                    self.animations.clear();
                    self.indexed.clear();
                    self.pending = None;
                }
                native.extend_from_slice(bytes);
            }
        });
        if !native.is_empty() {
            terminal.vt_write(&native);
        }
        native.clear();
        self.native_buffer = native;
        self.filter = filter;
    }

    fn command(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        bytes: &[u8],
        reply: &mut dyn FnMut(&[u8]),
    ) {
        let (controls, payload) = bytes.split_once_byte(b';');
        let fields_iter = || controls.split(|byte| *byte == b',');
        let extended = self.pending.is_some()
            || (fields_iter().any(|field| field.starts_with(b"i="))
                && fields_iter().any(|field| field.starts_with(b"I=")))
            || fields_iter().any(|field| {
                matches!(
                    field,
                    b"a=f"
                        | b"a=a"
                        | b"a=c"
                        | b"d=f"
                        | b"d=F"
                        | b"f=8"
                        | b"f=16"
                        | b"t=f"
                        | b"t=t"
                        | b"t=s"
                ) || field.starts_with(b"e=")
            });
        if !extended {
            terminal.vt_write(b"\x1b_G");
            terminal.vt_write(bytes);
            terminal.vt_write(b"\x1b\\");
            return;
        }
        self.prune(terminal);
        let Ok(mut fields) = Fields::parse(controls) else {
            return;
        };
        let action = fields.character(b'a', b't');
        if fields.number(b'i').unwrap_or(0) != 0 && fields.number(b'I').unwrap_or(0) != 0 {
            respond(&fields, None, None, "EINVAL", reply);
            return;
        }
        if action == b'q' && fields.0.contains_key(&b'e') {
            feature_query(&fields, reply);
            return;
        }
        if matches!(action, b't' | b'T' | b'q')
            && matches!(fields.character(b't', b'd'), b'f' | b't' | b's')
            && !matches!(fields.number(b'f'), Ok(8 | 16))
        {
            let result = (|| {
                let payload = PAYLOAD_BASE64.decode(payload).map_err(|_| "EINVAL")?;
                if action == b'q' {
                    decode_image(&fields, payload)?;
                    respond(&fields, None, None, "OK", reply);
                } else {
                    let data = decode_transport(&fields, payload)?;
                    if data.is_empty() {
                        return Err("EINVAL");
                    }
                    let mut direct = fields.clone();
                    for key in b"tSOom" {
                        direct.0.remove(key);
                    }
                    upload_data(&direct, &data, |packet| terminal.vt_write(packet));
                }
                Ok::<_, &'static str>(())
            })();
            if let Err(error) = result {
                respond(&fields, None, None, error, reply);
            }
            return;
        }
        if action == b'd' {
            self.pending = None;
            if matches!(fields.character(b'd', b'a'), b'f' | b'F') {
                terminal.vt_write(&fields.packet(payload));
                self.delete_frame(terminal, &fields, reply);
                return;
            }
        }
        let indexed = matches!(fields.number(b'f'), Ok(8 | 16));
        let continuation = self.pending.as_ref().is_some_and(|pending| {
            matches!(action, b'f' | b't')
                && !fields.0.contains_key(&b'f')
                && [b'i', b'I'].into_iter().all(|key| {
                    fields.number(key).unwrap_or(u32::MAX) == 0
                        || fields.number(key).ok() == pending.fields.number(key).ok()
                })
        });
        if action == b'f' || indexed || continuation {
            let payload = match PAYLOAD_BASE64.decode(payload) {
                Ok(payload) => payload,
                Err(_) => {
                    let mut failed = self
                        .pending
                        .take()
                        .map(|pending| pending.fields)
                        .unwrap_or_else(|| fields.clone());
                    if fields.number(b'q').unwrap_or(0) != 0 {
                        failed.set(b'q', fields.number(b'q').unwrap_or(2));
                    }
                    respond(
                        &failed,
                        self.identify(terminal, &failed).ok(),
                        None,
                        "EINVAL",
                        reply,
                    );
                    return;
                }
            };
            let mut pending = if continuation {
                self.pending.take().unwrap()
            } else {
                Pending {
                    fields: fields.clone(),
                    payload: Vec::new(),
                }
            };
            if fields.number(b'q').unwrap_or(0) != 0 {
                pending.fields.set(b'q', fields.number(b'q').unwrap_or(2));
            }
            if pending.payload.len().saturating_add(payload.len()) > MAX_SEQUENCE_BYTES {
                respond(&pending.fields, None, None, "ENOSPC", reply);
                return;
            }
            pending.payload.extend(payload);
            if fields.number(b'm').unwrap_or(0) == 1 {
                self.pending = Some(pending);
                return;
            }
            fields = pending.fields;
            let result = if fields.character(b'a', b't') == b'f' {
                self.load_frame(terminal, &fields, pending.payload, reply)
            } else {
                self.load_indexed(terminal, config, &fields, pending.payload, reply)
            };
            if let Err(error) = result {
                let id = self.identify(terminal, &fields).ok();
                respond(&fields, id, None, error, reply);
            }
            return;
        }
        if action == b'a' || action == b'c' {
            if let Err(error) = self.animate(terminal, &fields, reply) {
                respond(
                    &fields,
                    self.identify(terminal, &fields).ok(),
                    None,
                    error,
                    reply,
                );
            }
            return;
        }
        terminal.vt_write(&fields.packet(payload));
    }

    fn identify(&self, terminal: &Terminal<'_, '_>, fields: &Fields) -> Result<u32, &'static str> {
        let image_id = fields.number(b'i')?;
        let number = fields.number(b'I')?;
        if image_id != 0 && number != 0 {
            return Err("EINVAL");
        }
        if image_id == 0 && number == 0 {
            return Err("EINVAL");
        }
        let graphics = terminal.kitty_graphics().map_err(|_| "ENOENT")?;
        if image_id != 0 {
            graphics.image(image_id).ok_or("ENOENT")?;
            Ok(image_id)
        } else {
            graphics
                .image_by_number(number)
                .ok_or("ENOENT")?
                .id()
                .map_err(|_| "ENOENT")
        }
    }

    fn animation<'runtime>(
        &'runtime mut self,
        terminal: &Terminal<'_, '_>,
        image_id: u32,
    ) -> Result<&'runtime mut AnimatedImage, &'static str> {
        let graphics = terminal.kitty_graphics().map_err(|_| "ENOENT")?;
        let image = graphics.image(image_id).ok_or("ENOENT")?;
        let generation = image.generation().map_err(|_| "ENOENT")?;
        if self.indexed.contains_key(&generation) {
            return Err("EINVAL");
        }
        if !self.animations.contains_key(&generation) {
            let width = image.width().map_err(|_| "EINVAL")?;
            let height = image.height().map_err(|_| "EINVAL")?;
            let root = super::image_pixels(&image).map_err(|_| "EINVAL")?;
            let cache = HashMap::from([(
                root.rgba.as_ptr() as usize,
                (Arc::downgrade(&root.rgba), root.clone()),
            )]);
            self.animations.insert(
                generation,
                AnimatedImage {
                    screen: terminal.active_screen().map_err(|_| "EINVAL")?,
                    image_id,
                    animation: Animation::new(
                        width,
                        height,
                        root.rgba.clone(),
                        self.started.elapsed(),
                    )?,
                    revision: 1,
                    pixels: root,
                    cache,
                    visible: false,
                },
            );
        }
        let other_bytes = self
            .animations
            .iter()
            .filter(|(key, _)| **key != generation)
            .map(|(_, image)| image.animation.total_bytes())
            .sum::<usize>();
        let image = self.animations.get_mut(&generation).unwrap();
        image
            .animation
            .set_storage_limit(MAX_ANIMATION_BYTES.saturating_sub(other_bytes));
        Ok(image)
    }

    fn load_frame(
        &mut self,
        terminal: &Terminal<'_, '_>,
        fields: &Fields,
        payload: Vec<u8>,
        reply: &mut dyn FnMut(&[u8]),
    ) -> Result<(), &'static str> {
        let image_id = self.identify(terminal, fields)?;
        if self
            .animations
            .values()
            .map(|image| image.animation.total_bytes())
            .sum::<usize>()
            .saturating_add(payload.len())
            > MAX_ANIMATION_BYTES
        {
            return Err("ENOSPC");
        }
        let (width, height, rgba) = decode_image(fields, payload)?;
        let now = self.started.elapsed();
        let load = FrameLoad {
            base_frame: fields.number(b'c')?,
            edit_frame: fields.number(b'r')?,
            x: fields.number(b'x')?,
            y: fields.number(b'y')?,
            gap: fields.signed(b'z')?,
            replace: boolean(fields, b'X')?,
            background: fields.number(b'Y')?,
        };
        let image = self.animation(terminal, image_id)?;
        image
            .animation
            .load(load, rgba.into(), width, height, now)?;
        let frame = if load.edit_frame > 0 && load.edit_frame <= image.animation.frame_count() {
            load.edit_frame
        } else {
            image.animation.frame_count()
        };
        respond(fields, Some(image_id), Some(frame), "OK", reply);
        Ok(())
    }

    fn animate(
        &mut self,
        terminal: &Terminal<'_, '_>,
        fields: &Fields,
        reply: &mut dyn FnMut(&[u8]),
    ) -> Result<(), &'static str> {
        let image_id = self.identify(terminal, fields)?;
        let now = self.started.elapsed();
        if fields.character(b'a', b't') == b'a' {
            let control = Control {
                state: fields.number(b's')?,
                current_frame: fields.number(b'c')?,
                gap_frame: fields.number(b'r')?,
                gap: fields.signed(b'z')?,
                loops: fields.number(b'v')?,
            };
            self.animation(terminal, image_id)?
                .animation
                .control(control, now)?;
        } else {
            let compose = Compose {
                source_frame: fields.number(b'r')?,
                destination_frame: fields.number(b'c')?,
                source_x: fields.number(b'X')?,
                source_y: fields.number(b'Y')?,
                destination_x: fields.number(b'x')?,
                destination_y: fields.number(b'y')?,
                width: fields.number(b'w')?,
                height: fields.number(b'h')?,
                replace: boolean(fields, b'C')?,
            };
            self.animation(terminal, image_id)?
                .animation
                .compose(compose, now)?;
            respond(fields, Some(image_id), None, "OK", reply);
        }
        Ok(())
    }

    fn delete_frame(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        fields: &Fields,
        reply: &mut dyn FnMut(&[u8]),
    ) {
        let result = (|| {
            let image_id = self.identify(terminal, fields)?;
            let now = self.started.elapsed();
            let delete_last = fields.character(b'd', b'f') == b'F';
            let remove = self.animation(terminal, image_id)?.animation.delete_frame(
                fields.number(b'r')?,
                delete_last,
                now,
            )?;
            if remove {
                terminal.vt_write(format!("\x1b_Ga=d,d=I,i={image_id}\x1b\\").as_bytes());
            }
            Ok::<_, &'static str>(())
        })();
        if let Err(error) = result {
            respond(
                fields,
                self.identify(terminal, fields).ok(),
                None,
                error,
                reply,
            );
        }
    }

    fn load_indexed(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        config: &Config,
        fields: &Fields,
        payload: Vec<u8>,
        reply: &mut dyn FnMut(&[u8]),
    ) -> Result<(), &'static str> {
        let width = fields.number(b's')?;
        let height = fields.number(b'v')?;
        let format = fields.number(b'f')?;
        let channels = if format == 8 {
            1
        } else if format == 16 {
            2
        } else {
            return Err("EINVAL");
        };
        let data = decode_transport(fields, payload)?;
        let length = checked_length(width, height, channels)?;
        checked_length(width, height, 4)?;
        if data.len() != length {
            return Err("EINVAL");
        }
        if fields.character(b'a', b't') == b'q' {
            respond(fields, None, None, "OK", reply);
            return Ok(());
        }
        if fields.number(b'i')? != 0 && fields.number(b'I')? != 0 {
            return Err("EINVAL");
        }
        let palette = palette(terminal, config).map_err(|_| "EINVAL")?;
        let rgba = expand_indices(&data, channels, &palette);
        let mut upload = fields.clone();
        for key in b"tSoOm" {
            upload.0.remove(key);
        }
        upload.set(b'f', 32);
        let anonymous = fields.number(b'i')? == 0 && fields.number(b'I')? == 0;
        if anonymous {
            while terminal
                .kitty_graphics()
                .map_err(|_| "EINVAL")?
                .image(self.next_id)
                .is_some()
            {
                self.next_id = self.next_id.wrapping_add(1).max(1);
            }
            upload.set(b'i', self.next_id);
            upload.set(b'q', 2);
        }
        upload_data(&upload, &rgba, |packet| terminal.vt_write(packet));
        let image_id = self.identify(terminal, &upload)?;
        let graphics = terminal.kitty_graphics().map_err(|_| "EINVAL")?;
        let image = graphics.image(image_id).ok_or("EINVAL")?;
        let generation = image.generation().map_err(|_| "EINVAL")?;
        self.indexed.insert(
            generation,
            IndexedImage {
                screen: terminal.active_screen().map_err(|_| "EINVAL")?,
                image_id,
                width,
                height,
                channels,
                indices: data,
                palette,
                pixels: super::new_pixels(width, height, rgba.into()),
            },
        );
        Ok(())
    }

    pub fn pixels(
        &mut self,
        terminal: &Terminal<'_, '_>,
        image: &Image<'_>,
        config: &Config,
    ) -> Result<Option<Arc<ImagePixels>>, Error> {
        let generation = image.generation()?;
        if let Some(image) = self.animations.get_mut(&generation) {
            let revision = image.animation.revision();
            if image.revision != revision {
                let rgba = image.animation.current_pixels();
                let key = rgba.as_ptr() as usize;
                image
                    .cache
                    .retain(|_, (pixels, _)| pixels.strong_count() > 1);
                image.pixels = image
                    .cache
                    .get(&key)
                    .filter(|(weak, _)| {
                        weak.upgrade()
                            .is_some_and(|cached| Arc::ptr_eq(&cached, &rgba))
                    })
                    .map(|(_, cached)| cached.clone())
                    .unwrap_or_else(|| {
                        let pixels = super::new_pixels(
                            image.pixels.width,
                            image.pixels.height,
                            rgba.clone(),
                        );
                        image
                            .cache
                            .insert(key, (Arc::downgrade(&rgba), pixels.clone()));
                        pixels
                    });
                image.revision = revision;
            }
            return Ok(Some(image.pixels.clone()));
        }
        if let Some(image) = self.indexed.get_mut(&generation) {
            let current_palette = palette(terminal, config)?;
            if image.palette != current_palette {
                image.pixels = super::new_pixels(
                    image.width,
                    image.height,
                    expand_indices(&image.indices, image.channels, &current_palette).into(),
                );
                image.palette = current_palette;
            }
            return Ok(Some(image.pixels.clone()));
        }
        Ok(None)
    }

    pub fn advance(&mut self, terminal: &Terminal<'_, '_>) -> Result<bool, Error> {
        if self.animations.is_empty() && self.indexed.is_empty() {
            return Ok(false);
        }
        self.prune(terminal);
        let screen = terminal.active_screen()?;
        let now = self.started.elapsed();
        let mut changed = false;
        for image in self
            .animations
            .values_mut()
            .filter(|image| image.screen == screen && image.visible)
        {
            changed |= image.animation.advance(now);
        }
        Ok(changed)
    }

    fn prune(&mut self, terminal: &Terminal<'_, '_>) {
        if self.animations.is_empty() && self.indexed.is_empty() {
            return;
        }
        let Ok(screen) = terminal.active_screen() else {
            return;
        };
        let Ok(graphics) = terminal.kitty_graphics() else {
            return;
        };
        self.animations.retain(|generation, image| {
            image.screen != screen
                || graphics
                    .image(image.image_id)
                    .is_some_and(|root| root.generation().ok() == Some(*generation))
        });
        self.indexed.retain(|generation, image| {
            image.screen != screen
                || graphics
                    .image(image.image_id)
                    .is_some_and(|root| root.generation().ok() == Some(*generation))
        });
    }

    pub fn deadline(&self, terminal: &Terminal<'_, '_>) -> Option<Instant> {
        if self.animations.is_empty() {
            return None;
        }
        let screen = terminal.active_screen().ok()?;
        self.animations
            .values()
            .filter(|image| image.screen == screen && image.visible)
            .filter_map(|image| image.animation.next_deadline())
            .min()
            .map(|deadline| self.started + deadline)
    }

    pub fn set_visible(
        &mut self,
        terminal: &Terminal<'_, '_>,
        images: &HashSet<u32>,
    ) -> Result<(), Error> {
        if self.animations.is_empty() {
            return Ok(());
        }
        let screen = terminal.active_screen()?;
        for image in self
            .animations
            .values_mut()
            .filter(|image| image.screen == screen)
        {
            image.visible = images.contains(&image.image_id);
        }
        Ok(())
    }
}

fn boolean(fields: &Fields, key: u8) -> Result<bool, &'static str> {
    match fields.number(key)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("EINVAL"),
    }
}

fn respond(
    fields: &Fields,
    id: Option<u32>,
    frame: Option<u32>,
    code: &str,
    reply: &mut dyn FnMut(&[u8]),
) {
    let quiet = fields.number(b'q').unwrap_or(2);
    if quiet >= 2 || (quiet == 1 && code == "OK") {
        return;
    }
    let image_id = id.unwrap_or_else(|| fields.number(b'i').unwrap_or(0));
    let number = fields.number(b'I').unwrap_or(0);
    if image_id == 0 && number == 0 {
        return;
    }
    let mut response = format!("\x1b_Gi={image_id}");
    if number != 0 {
        response.push_str(&format!(",I={number}"));
    }
    if let Some(frame) = frame {
        response.push_str(&format!(",r={frame}"));
    } else if fields.number(b'p').unwrap_or(0) != 0 {
        response.push_str(&format!(",p={}", fields.number(b'p').unwrap()));
    }
    let message = if code == "EBADF" {
        "EBADF:Failed to read image file"
    } else {
        code
    };
    response.push_str(&format!(";{message}\x1b\\"));
    reply(response.as_bytes());
}

fn feature_query(fields: &Fields, reply: &mut dyn FnMut(&[u8])) {
    let Ok(image_id) = fields.number(b'i') else {
        return;
    };
    if image_id == 0 || fields.number(b'I').unwrap_or(0) != 0 {
        return;
    }
    let action = fields.character(b'e', 0);
    let keys: &[u8] = match action {
        b't' | b'q' => b"ftsvSOopN",
        b'T' => b"ftsvSOopNxywhXYcrCUzPQHV",
        b'p' => b"pxywhXYcrCUzPQHV",
        b'f' => b"ftsvSOoNxycrzXY",
        b'a' => b"srzcv",
        b'c' => b"crxywhXYC",
        b'd' => b"dpxyzr",
        _ => b"",
    };
    let mut response = format!("\x1b_Gi={image_id}");
    if !keys.is_empty() {
        response.push_str(&format!(",e={}", action as char));
        for (key, value) in &fields.0 {
            if !keys.contains(key) {
                continue;
            }
            let default = match key {
                b'f' => "32",
                b't' => "d",
                b'd' => "a",
                _ => "0",
            };
            if value == default {
                continue;
            }
            let supported = match *key {
                b'f' => {
                    matches!(value.as_str(), "24" | "32" | "100")
                        || (action != b'f' && matches!(value.as_str(), "8" | "16"))
                }
                b't' => matches!(value.as_str(), "d" | "f" | "t" | "s"),
                b'o' => value == "z",
                b'C' | b'U' => value == "1",
                b'X' if action == b'f' => value == "1",
                b's' if action == b'a' => matches!(value.as_str(), "1" | "2" | "3"),
                b'd' => {
                    value.len() == 1 && b"aAcCfFiInNpPqQrRxXyYzZ".contains(&value.as_bytes()[0])
                }
                b'z' | b'H' | b'V' => value.parse::<i32>().is_ok(),
                _ => value.parse::<u32>().is_ok(),
            };
            if supported {
                response.push_str(&format!(",{}={value}", *key as char));
            }
        }
    }
    response.push_str(";OK\x1b\\");
    if fields.number(b'q').unwrap_or(0) == 0 {
        reply(response.as_bytes());
    }
}

fn checked_length(width: u32, height: u32, channels: usize) -> Result<usize, &'static str> {
    let bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(height as usize))
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or("ENOSPC")?;
    if bytes == 0 || bytes > MAX_DECODED_BYTES {
        return Err("ENOSPC");
    }
    Ok(bytes)
}

fn decode_image(fields: &Fields, payload: Vec<u8>) -> Result<(u32, u32, Vec<u8>), &'static str> {
    let data = decode_transport(fields, payload)?;
    match fields.number(b'f')? {
        0 | 24 | 32 => {
            let width = fields.number(b's')?;
            let height = fields.number(b'v')?;
            let format = if fields.number(b'f')? == 24 {
                ImageFormat::Rgb
            } else {
                ImageFormat::Rgba
            };
            checked_length(width, height, 4)?;
            let rgba = normalize_pixels(width, height, format, &data).ok_or("EINVAL")?;
            Ok((width, height, rgba))
        }
        100 => {
            let mut decoder = png::Decoder::new(Cursor::new(data));
            decoder
                .set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
            let mut reader = decoder.read_info().map_err(|_| "EBADPNG")?;
            let length = reader.output_buffer_size().ok_or("ENOSPC")?;
            if length > MAX_DECODED_BYTES {
                return Err("ENOSPC");
            }
            let mut data = vec![0; length];
            let info = reader.next_frame(&mut data).map_err(|_| "EBADPNG")?;
            let format = match info.color_type {
                png::ColorType::Rgb => ImageFormat::Rgb,
                png::ColorType::Rgba => ImageFormat::Rgba,
                png::ColorType::Grayscale => ImageFormat::Gray,
                png::ColorType::GrayscaleAlpha => ImageFormat::GrayAlpha,
                _ => return Err("EBADPNG"),
            };
            let rgba =
                normalize_pixels(info.width, info.height, format, &data[..info.buffer_size()])
                    .ok_or("EBADPNG")?;
            Ok((info.width, info.height, rgba))
        }
        _ => Err("EINVAL"),
    }
}

fn decode_transport(fields: &Fields, payload: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    let medium = fields.character(b't', b'd');
    let data = match medium {
        b'd' => payload,
        b'f' | b't' | b's' => {
            if payload.contains(&0) {
                return Err("EBADF");
            }
            let path = PathBuf::from(OsStr::from_bytes(&payload));
            let mut remove_temporary = None;
            let mut file = if medium == b's' {
                let name = CString::new(payload).map_err(|_| "EBADF")?;
                let descriptor = unsafe {
                    libc::shm_open(
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
                        0,
                    )
                };
                if descriptor < 0 {
                    return Err("EBADF");
                }
                let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
                unsafe {
                    libc::shm_unlink(name.as_ptr());
                }
                File::from(descriptor)
            } else {
                let resolved = path.canonicalize().map_err(|_| "EBADF")?;
                if ["/proc", "/sys", "/dev"]
                    .iter()
                    .any(|prefix| resolved.starts_with(prefix))
                    && !resolved.starts_with("/dev/shm")
                {
                    return Err("EBADF");
                }
                if !fs::metadata(&resolved).map_err(|_| "EBADF")?.is_file() {
                    return Err("EBADF");
                }
                let file = File::options()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&resolved)
                    .map_err(|_| "EBADF")?;
                let temporary = medium == b't'
                    && resolved
                        .as_os_str()
                        .as_bytes()
                        .windows(b"tty-graphics-protocol".len())
                        .any(|part| part == b"tty-graphics-protocol")
                    && [
                        PathBuf::from("/tmp"),
                        PathBuf::from("/dev/shm"),
                        std::env::temp_dir(),
                    ]
                    .iter()
                    .any(|directory| {
                        directory
                            .canonicalize()
                            .is_ok_and(|directory| resolved.starts_with(directory))
                    });
                if temporary {
                    remove_temporary = Some(resolved);
                }
                file
            };
            let read = || -> Result<Vec<u8>, &'static str> {
                if !file.metadata().map_err(|_| "EBADF")?.is_file() {
                    return Err("EBADF");
                }
                file.seek(SeekFrom::Start(u64::from(fields.number(b'O')?)))
                    .map_err(|_| "EBADF")?;
                let mut size = fields.number(b'S')? as usize;
                if fields.character(b'o', 0) == 0 {
                    let channels = match fields.number(b'f')? {
                        0 | 32 => Some(4),
                        24 => Some(3),
                        8 => Some(1),
                        16 => Some(2),
                        _ => None,
                    };
                    if let Some(channels) = channels {
                        let expected =
                            checked_length(fields.number(b's')?, fields.number(b'v')?, channels)?;
                        if size == 0 {
                            size = expected;
                        } else if size < expected {
                            return Err("EBADF");
                        }
                    }
                }
                if size > MAX_SEQUENCE_BYTES {
                    return Err("EBADF");
                }
                let mut data = Vec::new();
                file.take(if size == 0 {
                    MAX_SEQUENCE_BYTES + 1
                } else {
                    size
                } as u64)
                    .read_to_end(&mut data)
                    .map_err(|_| "EBADF")?;
                if data.len() > MAX_SEQUENCE_BYTES || (size > 0 && data.len() != size) {
                    return Err("EBADF");
                }
                Ok(data)
            };
            let result = read();
            if let Some(temporary) = remove_temporary {
                let _ = fs::remove_file(temporary);
            }
            result?
        }
        _ => return Err("EINVAL"),
    };
    match fields.character(b'o', 0) {
        0 => Ok(data),
        b'z' => {
            let mut decoded = Vec::new();
            flate2::read::ZlibDecoder::new(data.as_slice())
                .take(MAX_DECODED_BYTES as u64 + 1)
                .read_to_end(&mut decoded)
                .map_err(|_| "EINVAL")?;
            if decoded.len() > MAX_DECODED_BYTES {
                return Err("ENOSPC");
            }
            Ok(decoded)
        }
        _ => Err("EINVAL"),
    }
}

fn palette(terminal: &Terminal<'_, '_>, config: &Config) -> Result<Vec<[u8; 4]>, Error> {
    let palette = terminal.color_palette()?;
    let mut colors = (0..=255)
        .map(|index| {
            let color = palette.get(PaletteIndex(index));
            [color.r, color.g, color.b, 255]
        })
        .collect::<Vec<_>>();
    let foreground = terminal
        .fg_color()?
        .unwrap_or(crate::terminal::Rgb::from(config.colors.foreground).into());
    let background = terminal
        .bg_color()?
        .unwrap_or(crate::terminal::Rgb::from(config.colors.background).into());
    let cursor = terminal
        .cursor_color()?
        .unwrap_or(crate::terminal::Rgb::from(config.colors.cursor).into());
    colors[248] = [background.r, background.g, background.b, 255];
    colors[249] = [foreground.r, foreground.g, foreground.b, 255];
    colors[250] = [
        config.colors.selection_background.red,
        config.colors.selection_background.green,
        config.colors.selection_background.blue,
        255,
    ];
    colors[251] = [
        config.colors.selection_foreground.red,
        config.colors.selection_foreground.green,
        config.colors.selection_foreground.blue,
        255,
    ];
    colors[252] = [cursor.r, cursor.g, cursor.b, 255];
    colors[253] = colors[248];
    colors[254] = colors[249];
    colors[255] = colors[249];
    Ok(colors)
}

fn expand_indices(indices: &[u8], channels: usize, palette: &[[u8; 4]]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(indices.len() / channels * 4);
    for pixel in indices.chunks_exact(channels) {
        let mut color = palette[pixel[0] as usize];
        if channels == 2 {
            color[3] = pixel[1];
        }
        rgba.extend_from_slice(&color);
    }
    rgba
}

fn upload_data(fields: &Fields, data: &[u8], mut emit: impl FnMut(&[u8])) {
    let chunks = data.chunks(3072);
    let count = chunks.len();
    if count == 0 {
        return;
    }
    let mut controls = fields.clone();
    controls.set(b'm', u8::from(count > 1));
    let mut header = controls.packet(&[]);
    header.truncate(header.len() - 2);
    let mut packet = Vec::with_capacity(header.len() + 4098);
    for (index, chunk) in chunks.enumerate() {
        packet.clear();
        packet.extend_from_slice(if index == 0 {
            &header
        } else if index + 1 < count {
            b"\x1b_Gm=1;"
        } else {
            b"\x1b_Gm=0;"
        });
        let start = packet.len();
        packet.resize(start + chunk.len().div_ceil(3) * 4, 0);
        STANDARD.encode_slice(chunk, &mut packet[start..]).unwrap();
        packet.extend_from_slice(b"\x1b\\");
        emit(&packet);
    }
}

trait SplitByte {
    fn split_once_byte(&self, byte: u8) -> (&[u8], &[u8]);
}

impl SplitByte for [u8] {
    fn split_once_byte(&self, byte: u8) -> (&[u8], &[u8]) {
        memchr::memchr(byte, self).map_or((self, &[]), |position| {
            (&self[..position], &self[position + 1..])
        })
    }
}

#[derive(Default)]
struct Filter {
    state: FilterState,
    buffer: Vec<u8>,
}

#[derive(Default)]
enum FilterState {
    #[default]
    Ground,
    Escape,
    ApcStart,
    Graphics,
    GraphicsEscape,
    String {
        osc: bool,
        escaped: bool,
    },
    Discard {
        escaped: bool,
    },
}

impl Filter {
    fn write(&mut self, bytes: &[u8], mut emit: impl FnMut(bool, &[u8])) {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            match self.state {
                FilterState::Ground => {
                    let end = memchr::memchr(0x1b, remaining).unwrap_or(remaining.len());
                    if end > 0 {
                        emit(false, &remaining[..end]);
                    }
                    remaining = &remaining[end..];
                    if !remaining.is_empty() {
                        self.state = FilterState::Escape;
                        remaining = &remaining[1..];
                    }
                }
                FilterState::Escape => {
                    let byte = remaining[0];
                    remaining = &remaining[1..];
                    if byte == b'_' {
                        self.state = FilterState::ApcStart;
                    } else if byte == 0x1b {
                        emit(false, &[0x1b]);
                        self.state = FilterState::Escape;
                    } else {
                        emit(false, &[0x1b, byte]);
                        self.state = match byte {
                            b']' => FilterState::String {
                                osc: true,
                                escaped: false,
                            },
                            b'P' | b'^' | b'X' => FilterState::String {
                                osc: false,
                                escaped: false,
                            },
                            _ => FilterState::Ground,
                        };
                    }
                }
                FilterState::ApcStart => {
                    if remaining[0] == b'G' {
                        self.buffer.clear();
                        self.state = FilterState::Graphics;
                    } else {
                        emit(false, &[0x1b, b'_', remaining[0]]);
                        self.state = FilterState::String {
                            osc: false,
                            escaped: remaining[0] == 0x1b,
                        };
                    }
                    remaining = &remaining[1..];
                }
                FilterState::Graphics => {
                    let end =
                        memchr::memchr3(0x1b, 0x18, 0x1a, remaining).unwrap_or(remaining.len());
                    if self.buffer.len().saturating_add(end) > MAX_SEQUENCE_BYTES {
                        self.buffer.clear();
                        remaining = &remaining[end..];
                        if let Some(byte) = remaining.first() {
                            self.state = FilterState::Discard {
                                escaped: *byte == 0x1b,
                            };
                            remaining = &remaining[1..];
                        } else {
                            self.state = FilterState::Discard { escaped: false };
                        }
                        continue;
                    } else {
                        self.buffer.extend_from_slice(&remaining[..end]);
                    }
                    remaining = &remaining[end..];
                    if let Some(byte) = remaining.first() {
                        self.state = if *byte == 0x1b {
                            FilterState::GraphicsEscape
                        } else {
                            self.buffer.clear();
                            FilterState::Ground
                        };
                        remaining = &remaining[1..];
                    }
                }
                FilterState::GraphicsEscape => {
                    if remaining[0] == b'\\' {
                        emit(true, &self.buffer);
                    }
                    self.buffer.clear();
                    if self.buffer.capacity() > 64 * 1024 {
                        self.buffer.shrink_to(64 * 1024);
                    }
                    self.state = FilterState::Ground;
                    remaining = &remaining[1..];
                }
                FilterState::String { osc, escaped } => {
                    if escaped {
                        let byte = remaining[0];
                        emit(false, &remaining[..1]);
                        remaining = &remaining[1..];
                        self.state = if byte == b'\\' {
                            FilterState::Ground
                        } else {
                            FilterState::String {
                                osc,
                                escaped: byte == 0x1b,
                            }
                        };
                    } else {
                        let end = memchr::memchr3(0x1b, 0x18, 0x1a, remaining)
                            .into_iter()
                            .chain(osc.then(|| memchr::memchr(7, remaining)).flatten())
                            .min()
                            .unwrap_or(remaining.len());
                        if end > 0 {
                            emit(false, &remaining[..end]);
                        }
                        remaining = &remaining[end..];
                        if let Some(byte) = remaining.first() {
                            emit(false, &remaining[..1]);
                            self.state = if *byte == 0x1b {
                                FilterState::String { osc, escaped: true }
                            } else {
                                FilterState::Ground
                            };
                            remaining = &remaining[1..];
                        }
                    }
                }
                FilterState::Discard { escaped } => {
                    if escaped && remaining[0] == b'\\' {
                        self.state = FilterState::Ground;
                    } else {
                        self.state = FilterState::Discard {
                            escaped: remaining[0] == 0x1b,
                        };
                    }
                    remaining = &remaining[1..];
                }
            }
        }
    }
}

fn needs_filter(bytes: &[u8]) -> bool {
    bytes.last() == Some(&0x1b)
        || [
            b"\x1b_",
            b"\x1b]",
            b"\x1bP",
            b"\x1b^",
            b"\x1bX",
            b"\x1bc",
            b"\x1b\x1b",
        ]
        .iter()
        .any(|sequence| memchr::memmem::find(bytes, *sequence).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(chunks: &[&[u8]]) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut filter = Filter::default();
        let mut plain = Vec::new();
        let mut graphics = Vec::new();
        for chunk in chunks {
            filter.write(chunk, |is_graphics, bytes| {
                if is_graphics {
                    graphics.push(bytes.to_vec());
                } else {
                    plain.extend_from_slice(bytes);
                }
            });
        }
        (plain, graphics)
    }

    #[test]
    fn graphics_filter_preserves_all_chunk_boundaries_and_plain_vt_bytes() {
        let bytes =
            b"hello\x1b[1;31mworld\x1b_Ga=f,i=7,m=1;YWJj\x1b\\next\x1b_Ga=a,i=7,c=2\x1b\\done";
        let expected = (
            b"hello\x1b[1;31mworldnextdone".to_vec(),
            vec![b"a=f,i=7,m=1;YWJj".to_vec(), b"a=a,i=7,c=2".to_vec()],
        );
        for split in 0..=bytes.len() {
            assert_eq!(
                filtered(&[&bytes[..split], &bytes[split..]]),
                expected,
                "split {split}"
            );
        }
        for size in 1..=32 {
            assert_eq!(
                filtered(&bytes.chunks(size).collect::<Vec<_>>()),
                expected,
                "chunk {size}"
            );
        }
    }

    #[test]
    fn graphics_inside_other_strings_are_never_executed() {
        for (introducer, terminator) in [
            (b']', b"\x07".as_slice()),
            (b'P', b"\x1b\\"),
            (b'^', b"\x1b\\"),
            (b'_', b"\x1b\\"),
        ] {
            let mut bytes = vec![0x1b, introducer, b'x'];
            bytes.extend_from_slice(b"\x1b_Ga=d,d=A");
            bytes.extend_from_slice(terminator);
            let expected_plain = bytes.clone();
            bytes.extend_from_slice(b"\x1b_Ga=a,i=1\x1b\\");
            let chunks = bytes.chunks(1).collect::<Vec<_>>();
            let (plain, graphics) = filtered(&chunks);
            assert_eq!(graphics, vec![b"a=a,i=1".to_vec()]);
            assert_eq!(plain, expected_plain);
        }
    }

    #[test]
    fn cancel_and_repeated_escape_do_not_leak_partial_graphics_commands() {
        let (plain, graphics) = filtered(&[b"\x1b_Ga=f,i=2;bad\x18text\x1b\x1b_Ga=a,i=3\x1b\\"]);
        assert_eq!(plain, b"text\x1b");
        assert_eq!(graphics, vec![b"a=a,i=3".to_vec()]);
    }

    fn protocol_terminal() -> (Terminal<'static, 'static>, Config, Protocol) {
        let config = Config::default();
        let mut terminal = Terminal::new(libghostty_vt::TerminalOptions {
            cols: 8,
            rows: 4,
            max_scrollback: 0,
        })
        .unwrap();
        crate::terminal::apply_theme(&mut terminal, &config).unwrap();
        (terminal, config, Protocol::new())
    }

    fn protocol_command(
        protocol: &mut Protocol,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        controls: &[u8],
        payload: &[u8],
    ) -> Vec<u8> {
        let packet = Fields::parse(controls)
            .unwrap()
            .packet(STANDARD.encode(payload).as_bytes());
        let mut replies = Vec::new();
        protocol.write(terminal, config, &packet, &mut |reply| {
            replies.extend_from_slice(reply)
        });
        replies
    }

    #[test]
    fn upload_packets_preserve_chunk_limits_controls_and_exact_payload() {
        let fields = Fields::parse(b"a=T,i=29,p=7,f=32,s=1024,v=16,q=2,m=99").unwrap();
        for length in [0_usize, 1, 2, 3, 3071, 3072, 3073, 6143, 6144, 6145, 65537] {
            let data = (0..length)
                .map(|index| index.wrapping_mul(37) as u8)
                .collect::<Vec<_>>();
            let mut packets = Vec::new();
            upload_data(&fields, &data, |packet| packets.push(packet.to_vec()));
            assert_eq!(packets.len(), length.div_ceil(3072));
            let mut decoded = Vec::new();
            for (index, packet) in packets.iter().enumerate() {
                assert!(packet.starts_with(b"\x1b_G"));
                assert!(packet.ends_with(b"\x1b\\"));
                let (controls, payload) = packet[3..packet.len() - 2].split_once_byte(b';');
                let parsed = Fields::parse(controls).unwrap();
                let mut expected = if index == 0 {
                    fields.clone()
                } else {
                    Fields::default()
                };
                expected.set(b'm', u8::from(index + 1 < packets.len()));
                assert_eq!(parsed.0, expected.0);
                assert!(payload.len() <= 4096);
                if index + 1 < packets.len() {
                    assert_eq!(payload.len(), 4096);
                }
                decoded.extend(STANDARD.decode(payload).unwrap());
            }
            assert_eq!(decoded, data);
        }
    }

    #[test]
    fn animation_continuations_with_repeated_image_ids_inherit_metadata_and_quiet() {
        let (mut terminal, config, mut protocol) = protocol_terminal();
        protocol_command(
            &mut protocol,
            &mut terminal,
            &config,
            b"a=t,i=7,f=32,s=2,v=1,q=2",
            &[255, 0, 0, 255, 255, 0, 0, 255],
        );
        assert!(
            protocol_command(
                &mut protocol,
                &mut terminal,
                &config,
                b"a=f,i=7,f=32,s=2,v=1,z=77,q=1,m=1",
                &[0, 255, 0, 255],
            )
            .is_empty()
        );
        assert!(protocol.pending.is_some());
        assert!(
            protocol_command(
                &mut protocol,
                &mut terminal,
                &config,
                b"a=f,i=7,m=0",
                &[0, 0, 255, 255],
            )
            .is_empty()
        );
        assert!(protocol.pending.is_none());
        assert!(
            protocol_command(&mut protocol, &mut terminal, &config, b"a=a,i=7,c=2", &[],)
                .is_empty()
        );
        let image = protocol.animations.values().next().unwrap();
        assert_eq!(image.animation.frame_count(), 2);
        assert_eq!(
            image.animation.current_pixels().as_ref(),
            &[0, 255, 0, 255, 0, 0, 255, 255]
        );
    }

    #[test]
    fn animation_errors_by_image_number_report_the_resolved_image_id() {
        let (mut terminal, config, mut protocol) = protocol_terminal();
        protocol_command(
            &mut protocol,
            &mut terminal,
            &config,
            b"a=t,I=901,f=32,s=1,v=1,q=2",
            &[255, 0, 0, 255],
        );
        let image_id = terminal
            .kitty_graphics()
            .unwrap()
            .image_by_number(901)
            .unwrap()
            .id()
            .unwrap();
        let reply = protocol_command(
            &mut protocol,
            &mut terminal,
            &config,
            b"a=c,I=901,r=999,c=1",
            &[],
        );
        assert_eq!(
            reply,
            format!("\x1b_Gi={image_id},I=901;ENOENT\x1b\\").as_bytes()
        );
    }

    #[test]
    fn animations_without_visible_placements_do_not_schedule_redraws() {
        let (mut terminal, config, mut protocol) = protocol_terminal();
        protocol_command(
            &mut protocol,
            &mut terminal,
            &config,
            b"a=t,i=7,f=32,s=1,v=1",
            &[255, 0, 0, 255],
        );
        protocol_command(
            &mut protocol,
            &mut terminal,
            &config,
            b"a=f,i=7,f=32,s=1,v=1,z=30",
            &[0, 255, 0, 255],
        );
        protocol_command(&mut protocol, &mut terminal, &config, b"a=a,i=7,s=3", &[]);
        assert!(protocol.deadline(&terminal).is_none());
        protocol
            .set_visible(&terminal, &HashSet::from([7]))
            .unwrap();
        assert!(protocol.deadline(&terminal).is_some());
        protocol.set_visible(&terminal, &HashSet::new()).unwrap();
        assert!(protocol.deadline(&terminal).is_none());
        assert!(!protocol.advance(&terminal).unwrap());
        protocol
            .set_visible(&terminal, &HashSet::from([7]))
            .unwrap();
        assert!(protocol.deadline(&terminal).is_some());
    }

    #[test]
    fn split_terminal_reset_releases_both_screen_models_and_pending_frame_data() {
        let (mut terminal, config, mut protocol) = protocol_terminal();
        for screen in [b"".as_slice(), b"\x1b[?1049h"] {
            protocol.write(&mut terminal, &config, screen, &mut |_| {});
            protocol_command(
                &mut protocol,
                &mut terminal,
                &config,
                b"a=t,i=17,f=32,s=1,v=1,q=2",
                &[255, 0, 0, 255],
            );
            protocol_command(
                &mut protocol,
                &mut terminal,
                &config,
                b"a=f,i=17,f=32,s=1,v=1,q=2",
                &[0, 255, 0, 255],
            );
            protocol_command(
                &mut protocol,
                &mut terminal,
                &config,
                b"a=t,i=18,f=8,s=1,v=1,q=2",
                &[1],
            );
        }
        assert_eq!(protocol.animations.len(), 2);
        assert_eq!(protocol.indexed.len(), 2);
        protocol_command(
            &mut protocol,
            &mut terminal,
            &config,
            b"a=f,i=17,f=32,s=1,v=1,q=2,m=1",
            &[0, 0, 255],
        );
        assert!(protocol.pending.is_some());
        protocol.write(&mut terminal, &config, b"\x1b", &mut |_| {});
        protocol.write(&mut terminal, &config, b"c", &mut |_| {});
        assert!(protocol.pending.is_none());
        assert!(protocol.animations.is_empty());
        assert!(protocol.indexed.is_empty());
        assert_eq!(terminal.active_screen().unwrap(), Screen::Primary);
        assert!(terminal.kitty_graphics().unwrap().image(17).is_none());
    }

    #[test]
    fn every_delete_action_aborts_an_incomplete_animation_transmission() {
        let (mut terminal, config, mut protocol) = protocol_terminal();
        protocol_command(
            &mut protocol,
            &mut terminal,
            &config,
            b"a=t,i=27,f=32,s=1,v=1,q=2",
            &[255, 0, 0, 255],
        );
        for deletion in [b"a=d,d=i,i=27".as_slice(), b"a=d,d=f,i=27"] {
            protocol_command(
                &mut protocol,
                &mut terminal,
                &config,
                b"a=f,i=27,f=32,s=1,v=1,q=2,m=1",
                &[0, 255, 0],
            );
            assert!(protocol.pending.is_some());
            protocol_command(&mut protocol, &mut terminal, &config, deletion, &[]);
            assert!(protocol.pending.is_none());
        }
        protocol_command(
            &mut protocol,
            &mut terminal,
            &config,
            b"a=f,i=27,f=32,s=1,v=1,q=2",
            &[0, 0, 255, 255],
        );
        let image = protocol.animations.values().next().unwrap();
        assert_eq!(image.animation.frame_count(), 2);
    }
}
