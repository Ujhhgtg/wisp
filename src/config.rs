use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::de::Error as DeserializeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const CONFIG_RELATIVE_PATH: &str = "wisp/config.toml";
const PROJECT_CONFIG_RELATIVE_PATH: &str = ".config/wisp/config.toml";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }

    pub const fn as_rgb(self) -> (u8, u8, u8) {
        (self.red, self.green, self.blue)
    }
}

impl Default for Color {
    fn default() -> Self {
        Self::new(255, 255, 255)
    }
}

impl Serialize for Color {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColorParseError {
    value: String,
    reason: String,
}

impl ColorParseError {
    fn new(value: &str, reason: impl Into<String>) -> Self {
        Self {
            value: value.to_owned(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ColorParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid color {:?}: {}", self.value, self.reason)
    }
}

impl std::error::Error for ColorParseError {}

impl FromStr for Color {
    type Err = ColorParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        if let Some(hex) = trimmed.strip_prefix('#') {
            return parse_hex_color(trimmed, hex);
        }

        if let Some(components) = trimmed.strip_prefix("rgb:") {
            return parse_rgb_colon(trimmed, components);
        }

        if let Some(components) = trimmed.strip_prefix("rgb(") {
            return parse_rgb_function(trimmed, components);
        }

        parse_named_color(trimmed)
    }
}

fn parse_hex_color(original: &str, value: &str) -> Result<Color, ColorParseError> {
    if !value.is_ascii() {
        return Err(ColorParseError::new(
            original,
            "contains a non-ASCII character",
        ));
    }

    match value.len() {
        3 => {
            let red = parse_hex_digit(value.as_bytes()[0], original)?;
            let green = parse_hex_digit(value.as_bytes()[1], original)?;
            let blue = parse_hex_digit(value.as_bytes()[2], original)?;
            Ok(Color::new(red * 17, green * 17, blue * 17))
        }
        6 => {
            let red = parse_hex_byte(&value[0..2], original)?;
            let green = parse_hex_byte(&value[2..4], original)?;
            let blue = parse_hex_byte(&value[4..6], original)?;
            Ok(Color::new(red, green, blue))
        }
        _ => Err(ColorParseError::new(original, "expected #rgb or #rrggbb")),
    }
}

fn parse_hex_digit(value: u8, original: &str) -> Result<u8, ColorParseError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(ColorParseError::new(original, "contains a non-hex digit")),
    }
}

fn parse_hex_byte(value: &str, original: &str) -> Result<u8, ColorParseError> {
    u8::from_str_radix(value, 16)
        .map_err(|_| ColorParseError::new(original, "contains a non-hex byte"))
}

fn parse_rgb_colon(original: &str, value: &str) -> Result<Color, ColorParseError> {
    let components: Vec<&str> = value.split('/').collect();
    if components.len() != 3 {
        return Err(ColorParseError::new(
            original,
            "expected rgb:rr/gg/bb or rgb:rrrr/gggg/bbbb",
        ));
    }

    let red = parse_scaled_hex_component(components[0], original)?;
    let green = parse_scaled_hex_component(components[1], original)?;
    let blue = parse_scaled_hex_component(components[2], original)?;
    Ok(Color::new(red, green, blue))
}

fn parse_scaled_hex_component(value: &str, original: &str) -> Result<u8, ColorParseError> {
    if value.is_empty() || value.len() > 4 {
        return Err(ColorParseError::new(
            original,
            "rgb: components must contain one to four hex digits",
        ));
    }

    let number = u32::from_str_radix(value, 16)
        .map_err(|_| ColorParseError::new(original, "rgb: contains a non-hex component"))?;
    let maximum = (1_u32 << (value.len() * 4)) - 1;
    Ok(((number * 255 + maximum / 2) / maximum) as u8)
}

fn parse_rgb_function(original: &str, value: &str) -> Result<Color, ColorParseError> {
    let Some(value) = value.strip_suffix(')') else {
        return Err(ColorParseError::new(
            original,
            "expected rgb(r, g, b) with a closing parenthesis",
        ));
    };
    let components: Vec<&str> = value.split(',').collect();
    if components.len() != 3 {
        return Err(ColorParseError::new(
            original,
            "expected three rgb components",
        ));
    }

    let red = parse_rgb_function_component(components[0], original)?;
    let green = parse_rgb_function_component(components[1], original)?;
    let blue = parse_rgb_function_component(components[2], original)?;
    Ok(Color::new(red, green, blue))
}

fn parse_rgb_function_component(value: &str, original: &str) -> Result<u8, ColorParseError> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        let number = percent
            .trim()
            .parse::<f32>()
            .map_err(|_| ColorParseError::new(original, "rgb percentage is not a number"))?;
        if !(0.0..=100.0).contains(&number) {
            return Err(ColorParseError::new(
                original,
                "rgb percentage must be between 0 and 100",
            ));
        }
        return Ok((number * 2.55).round() as u8);
    }

    let number = value
        .parse::<u16>()
        .map_err(|_| ColorParseError::new(original, "rgb component is not an integer"))?;
    u8::try_from(number)
        .map_err(|_| ColorParseError::new(original, "rgb component must be between 0 and 255"))
}

fn parse_named_color(value: &str) -> Result<Color, ColorParseError> {
    let color = match value.to_ascii_lowercase().as_str() {
        "black" => Color::new(0, 0, 0),
        "red" => Color::new(255, 0, 0),
        "green" => Color::new(0, 128, 0),
        "yellow" => Color::new(255, 255, 0),
        "blue" => Color::new(0, 0, 255),
        "magenta" | "fuchsia" => Color::new(255, 0, 255),
        "cyan" | "aqua" => Color::new(0, 255, 255),
        "white" => Color::new(255, 255, 255),
        "gray" | "grey" => Color::new(128, 128, 128),
        _ => {
            return Err(ColorParseError::new(
                value,
                "expected a named color or RGB value",
            ));
        }
    };
    Ok(color)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct FontConfig {
    #[serde(deserialize_with = "deserialize_font_family")]
    pub family: String,
    #[serde(deserialize_with = "deserialize_font_size")]
    pub size: f32,
    pub ligatures: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "monospace".to_owned(),
            size: 11.0,
            ligatures: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Margins {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Margins {
    pub const fn uniform(points: f32) -> Self {
        Self {
            top: points,
            right: points,
            bottom: points,
            left: points,
        }
    }

    pub fn validate(self) -> Result<(), ConfigValidationError> {
        if [self.top, self.right, self.bottom, self.left]
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err(ConfigValidationError(
                "window.margin_width must contain finite, non-negative point values".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Default for Margins {
    fn default() -> Self {
        Self::uniform(21.75)
    }
}

impl Serialize for Margins {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.top == self.right && self.top == self.bottom && self.top == self.left {
            serializer.serialize_f32(self.top)
        } else {
            [self.top, self.right, self.bottom, self.left].serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Margins {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Uniform(f32),
            Edges(Vec<f32>),
        }

        let margins = match Value::deserialize(deserializer)? {
            Value::Uniform(points) => Self::uniform(points),
            Value::Edges(edges) => match edges.as_slice() {
                [all] => Self::uniform(*all),
                [vertical, horizontal] => Self {
                    top: *vertical,
                    right: *horizontal,
                    bottom: *vertical,
                    left: *horizontal,
                },
                [top, horizontal, bottom] => Self {
                    top: *top,
                    right: *horizontal,
                    bottom: *bottom,
                    left: *horizontal,
                },
                [top, right, bottom, left] => Self {
                    top: *top,
                    right: *right,
                    bottom: *bottom,
                    left: *left,
                },
                _ => {
                    return Err(D::Error::custom(
                        "window.margin_width expects one to four values",
                    ));
                }
            },
        };
        margins.validate().map_err(D::Error::custom)?;
        Ok(margins)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WindowConfig {
    #[serde(alias = "window_margin_width")]
    pub margin_width: Margins,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

impl Default for CursorShape {
    fn default() -> Self {
        Self::Beam
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CursorConfig {
    pub shape: CursorShape,
    pub blink: bool,
    pub smooth: bool,
    pub trail: bool,
    pub trail_duration_ms: u32,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            shape: CursorShape::Beam,
            blink: true,
            smooth: true,
            trail: true,
            trail_duration_ms: 150,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct MouseConfig {
    pub enabled: bool,
    pub copy_on_select: bool,
    pub hide_when_typing: bool,
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            copy_on_select: false,
            hide_when_typing: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ProtocolConfig {
    pub kitty_keyboard: bool,
    pub kitty_graphics: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub hyperlinks: bool,
    pub clipboard_write: bool,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            kitty_keyboard: true,
            kitty_graphics: true,
            bracketed_paste: true,
            focus_events: true,
            hyperlinks: true,
            clipboard_write: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Colors {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    pub selection_background: Color,
    pub selection_foreground: Color,
    pub color0: Color,
    pub color1: Color,
    pub color2: Color,
    pub color3: Color,
    pub color4: Color,
    pub color5: Color,
    pub color6: Color,
    pub color7: Color,
    pub color8: Color,
    pub color9: Color,
    pub color10: Color,
    pub color11: Color,
    pub color12: Color,
    pub color13: Color,
    pub color14: Color,
    pub color15: Color,
}

impl Colors {
    pub fn ansi_palette(&self) -> [Color; 16] {
        [
            self.color0,
            self.color1,
            self.color2,
            self.color3,
            self.color4,
            self.color5,
            self.color6,
            self.color7,
            self.color8,
            self.color9,
            self.color10,
            self.color11,
            self.color12,
            self.color13,
            self.color14,
            self.color15,
        ]
    }
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            background: Color::new(0x00, 0x00, 0x00),
            foreground: Color::new(0xff, 0xff, 0xff),
            cursor: Color::new(0xff, 0xff, 0xff),
            selection_background: Color::new(0xff, 0xff, 0xff),
            selection_foreground: Color::new(0x00, 0x00, 0x00),
            color0: Color::new(0x00, 0x00, 0x00),
            color1: Color::new(0xcc, 0x00, 0x00),
            color2: Color::new(0x4e, 0x9a, 0x06),
            color3: Color::new(0xc4, 0xa0, 0x00),
            color4: Color::new(0x34, 0x65, 0xa4),
            color5: Color::new(0x75, 0x50, 0x7b),
            color6: Color::new(0x06, 0x98, 0x9a),
            color7: Color::new(0xd3, 0xd7, 0xcf),
            color8: Color::new(0x55, 0x57, 0x53),
            color9: Color::new(0xef, 0x29, 0x29),
            color10: Color::new(0x8a, 0xe2, 0x34),
            color11: Color::new(0xfc, 0xe9, 0x4f),
            color12: Color::new(0x72, 0x9f, 0xcf),
            color13: Color::new(0xad, 0x7f, 0xa8),
            color14: Color::new(0x34, 0xe2, 0xe2),
            color15: Color::new(0xee, 0xee, 0xec),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    #[serde(deserialize_with = "deserialize_scrollback_limit")]
    pub scrollback_limit: i64,
    pub font: FontConfig,
    pub window: WindowConfig,
    pub colors: Colors,
    pub cursor: CursorConfig,
    pub mouse: MouseConfig,
    pub protocols: ProtocolConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrollback_limit: 100_000,
            font: FontConfig::default(),
            window: WindowConfig::default(),
            colors: Colors::default(),
            cursor: CursorConfig::default(),
            mouse: MouseConfig::default(),
            protocols: ProtocolConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigValidationError(String);

impl fmt::Display for ConfigValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConfigValidationError {}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if !self.font.size.is_finite() || self.font.size <= 0.0 {
            return Err(ConfigValidationError(
                "font.size must be finite and greater than zero".to_owned(),
            ));
        }
        if self.font.family.trim().is_empty() {
            return Err(ConfigValidationError(
                "font.family cannot be empty".to_owned(),
            ));
        }
        if self.scrollback_limit < -1 {
            return Err(ConfigValidationError(
                "scrollback_limit must be -1 for infinite history or a non-negative limit"
                    .to_owned(),
            ));
        }
        self.window.margin_width.validate()
    }
}

fn deserialize_font_family<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let family = String::deserialize(deserializer)?;
    if family.trim().is_empty() {
        return Err(D::Error::custom("font.family cannot be empty"));
    }
    Ok(family)
}

fn deserialize_font_size<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    let size = f32::deserialize(deserializer)?;
    if !size.is_finite() || size <= 0.0 {
        return Err(D::Error::custom(
            "font.size must be finite and greater than zero",
        ));
    }
    Ok(size)
}

fn deserialize_scrollback_limit<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let limit = i64::deserialize(deserializer)?;
    if limit < -1 {
        return Err(D::Error::custom(
            "scrollback_limit must be -1 for infinite history or a non-negative limit",
        ));
    }
    Ok(limit)
}

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "failed to read {}: {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(formatter, "failed to parse {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

pub fn load() -> Result<Config, ConfigError> {
    for path in config_candidates() {
        match fs::read_to_string(&path) {
            Ok(contents) => {
                return toml::from_str(&contents)
                    .map_err(|source| ConfigError::Parse { path, source });
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(ConfigError::Io { path, source }),
        }
    }
    Ok(Config::default())
}

pub fn config_candidates() -> Vec<PathBuf> {
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    config_candidates_from(
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
        &current_dir,
    )
}

fn config_candidates_from(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    current_dir: &Path,
) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(3);
    if let Some(xdg_config_home) = non_empty_path(xdg_config_home) {
        push_unique(&mut candidates, xdg_config_home.join(CONFIG_RELATIVE_PATH));
    }

    if let Some(home) = non_empty_path(home) {
        push_unique(
            &mut candidates,
            home.join(".config").join(CONFIG_RELATIVE_PATH),
        );
    }
    push_unique(
        &mut candidates,
        current_dir.join(PROJECT_CONFIG_RELATIVE_PATH),
    );
    candidates
}

fn non_empty_path(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn defaults_match_requested_terminal_settings() {
        let config = Config::default();

        assert_eq!(config.scrollback_limit, 100_000);
        assert_eq!(config.font.family, "monospace");
        assert_eq!(config.font.size, 11.0);
        assert_eq!(config.window.margin_width, Margins::uniform(21.75));
        assert!(config.font.ligatures);
        assert_eq!(config.cursor.shape, CursorShape::Beam);
        assert!(config.cursor.smooth);
        assert!(config.cursor.trail);
        assert!(config.mouse.enabled);
        assert!(config.protocols.kitty_keyboard);
        assert!(config.protocols.kitty_graphics);
        assert_eq!(config.colors.background, Color::new(0, 0, 0));
        assert_eq!(config.colors.foreground, Color::new(255, 255, 255));
        assert_eq!(config.colors.ansi_palette().len(), 16);
    }

    #[test]
    fn parses_colors_in_common_terminal_formats() {
        assert_eq!("#1a2b3c".parse(), Ok(Color::new(0x1a, 0x2b, 0x3c)));
        assert_eq!("#abc".parse(), Ok(Color::new(0xaa, 0xbb, 0xcc)));
        assert_eq!("rgb:ffff/8000/0000".parse(), Ok(Color::new(255, 128, 0)));
        assert_eq!("rgb(26, 43, 60)".parse(), Ok(Color::new(26, 43, 60)));
        assert_eq!("rgb(10%, 20%, 30%)".parse(), Ok(Color::new(26, 51, 77)));
        assert_eq!("white".parse(), Ok(Color::new(255, 255, 255)));
        assert!("#12".parse::<Color>().is_err());
    }

    #[test]
    fn parses_toml_with_kitty_color_names_and_nested_defaults() {
        let config: Config = toml::from_str(
            r##"
scrollback_limit = 5000

[font]
family = "Iosevka Term"
size = 13.5

[colors]
background = "#101820"
color1 = "rgb:ffff/0000/0000"

[cursor]
shape = "underline"
trail = false

[protocols]
kitty_graphics = false
"##,
        )
        .expect("valid config");

        assert_eq!(config.scrollback_limit, 5000);
        assert_eq!(config.font.family, "Iosevka Term");
        assert_eq!(config.font.size, 13.5);
        assert!(config.font.ligatures);
        assert_eq!(config.colors.background, Color::new(0x10, 0x18, 0x20));
        assert_eq!(config.colors.color1, Color::new(255, 0, 0));
        assert_eq!(config.cursor.shape, CursorShape::Underline);
        assert!(!config.cursor.trail);
        assert!(!config.protocols.kitty_graphics);
        assert!(config.protocols.kitty_keyboard);
    }

    #[test]
    fn config_candidates_follow_requested_precedence() {
        let candidates = config_candidates_from(
            Some(OsString::from("/xdg")),
            Some(OsString::from("/home/tester")),
            Path::new("/project"),
        );

        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/xdg/wisp/config.toml"),
                PathBuf::from("/home/tester/.config/wisp/config.toml"),
                PathBuf::from("/project/.config/wisp/config.toml"),
            ]
        );
    }

    #[test]
    fn empty_xdg_home_is_ignored() {
        let candidates = config_candidates_from(
            Some(OsString::new()),
            Some(OsString::from("/home/tester")),
            Path::new("/project"),
        );

        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/home/tester/.config/wisp/config.toml"),
                PathBuf::from("/project/.config/wisp/config.toml"),
            ]
        );
    }

    #[test]
    fn kitty_margin_values_follow_vertical_horizontal_and_clockwise_order() {
        let expected = [
            ("9", [9.0, 9.0, 9.0, 9.0]),
            ("[9]", [9.0, 9.0, 9.0, 9.0]),
            ("[3, 6]", [3.0, 6.0, 3.0, 6.0]),
            ("[3, 6, 9]", [3.0, 6.0, 9.0, 6.0]),
            ("[3, 6, 9, 12]", [3.0, 6.0, 9.0, 12.0]),
        ];
        for (value, [top, right, bottom, left]) in expected {
            let config: Config = toml::from_str(&format!("[window]\nmargin_width = {value}"))
                .expect("valid margin configuration");
            assert_eq!(
                config.window.margin_width,
                Margins {
                    top,
                    right,
                    bottom,
                    left
                }
            );
        }
        let alias: Config = toml::from_str("[window]\nwindow_margin_width = 0").unwrap();
        assert_eq!(alias.window.margin_width, Margins::uniform(0.0));
    }

    #[test]
    fn config_rejects_invalid_font_scrollback_and_margin_values() {
        for source in [
            "[font]\nsize = 0",
            "[font]\nsize = -1",
            "[font]\nsize = nan",
            "[font]\nsize = inf",
            "[font]\nfamily = '   '",
            "scrollback_limit = -2",
            "[window]\nmargin_width = -1",
            "[window]\nmargin_width = nan",
            "[window]\nmargin_width = inf",
            "[window]\nmargin_width = [1, -1]",
            "[window]\nmargin_width = []",
            "[window]\nmargin_width = [1, 2, 3, 4, 5]",
        ] {
            assert!(toml::from_str::<Config>(source).is_err(), "{source}");
        }
        assert!(toml::from_str::<Config>("scrollback_limit = 0").is_ok());
    }

    #[test]
    fn margin_round_trip_preserves_uniform_and_asymmetric_values() {
        for margin in [
            Margins::default(),
            Margins {
                top: 1.0,
                right: 2.0,
                bottom: 3.0,
                left: 4.0,
            },
        ] {
            let mut config = Config::default();
            config.window.margin_width = margin;
            let source = toml::to_string(&config).unwrap();
            let decoded: Config = toml::from_str(&source).unwrap();
            assert_eq!(decoded.window.margin_width, margin);
        }
    }
}
