use std::env;
use std::fs;
use std::io;
use std::path::Path;

use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    English,
    SimplifiedChinese,
    TraditionalChinese,
}

impl Language {
    pub fn system() -> Self {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()))
            .unwrap_or_default();
        Self::from_locale(&locale)
    }

    pub fn from_locale(locale: &str) -> Self {
        let locale = locale.to_ascii_lowercase().replace('-', "_");
        let locale = locale.split(['.', '@']).next().unwrap_or_default();
        if locale == "zh" || locale.starts_with("zh_") {
            if locale.split('_').any(|part| part == "hans") {
                Self::SimplifiedChinese
            } else if locale
                .split('_')
                .any(|part| matches!(part, "hant" | "tw" | "hk" | "mo"))
            {
                Self::TraditionalChinese
            } else {
                Self::SimplifiedChinese
            }
        } else {
            Self::English
        }
    }

    pub fn text(self) -> DialogText {
        match self {
            Self::English => DialogText {
                title: "Close this window?",
                message: "A command or shell job is still running.\nClosing this window may terminate it.",
                close: "[Y] Close window",
                cancel: "[N] Keep open",
                help: "Tab / arrows: choose   Enter: confirm   Esc: keep open",
            },
            Self::SimplifiedChinese => DialogText {
                title: "关闭此窗口？",
                message: "仍有命令或 shell 任务正在运行。\n关闭窗口可能会终止这些任务。",
                close: "[Y] 关闭窗口",
                cancel: "[N] 保持打开",
                help: "Tab / 方向键：选择   Enter：确认   Esc：保持打开",
            },
            Self::TraditionalChinese => DialogText {
                title: "關閉此視窗？",
                message: "仍有命令或 shell 工作正在執行。\n關閉視窗可能會終止這些工作。",
                close: "[Y] 關閉視窗",
                cancel: "[N] 保持開啟",
                help: "Tab / 方向鍵：選擇   Enter：確認   Esc：保持開啟",
            },
        }
    }
}

pub struct DialogText {
    pub title: &'static str,
    pub message: &'static str,
    pub close: &'static str,
    pub cancel: &'static str,
    pub help: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Choice {
    Close,
    Cancel,
}

#[derive(Clone, Debug)]
pub struct CloseDialog {
    pub language: Language,
    pub selected: Choice,
    pressed: Option<Choice>,
}

impl CloseDialog {
    pub fn new(language: Language) -> Self {
        Self {
            language,
            selected: Choice::Cancel,
            pressed: None,
        }
    }

    pub fn key(&mut self, key: &Key) -> Option<Choice> {
        match key {
            Key::Named(NamedKey::Escape) => Some(Choice::Cancel),
            Key::Named(NamedKey::Enter) => Some(self.selected),
            Key::Named(
                NamedKey::Tab
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown,
            ) => {
                self.selected = match self.selected {
                    Choice::Close => Choice::Cancel,
                    Choice::Cancel => Choice::Close,
                };
                None
            }
            Key::Character(text) if text.eq_ignore_ascii_case("y") => Some(Choice::Close),
            Key::Character(text) if text.eq_ignore_ascii_case("n") => Some(Choice::Cancel),
            _ => None,
        }
    }

    pub fn hover(&mut self, choice: Option<Choice>) {
        if let Some(choice) = choice {
            self.selected = choice;
        }
    }

    pub fn mouse(&mut self, choice: Option<Choice>, pressed: bool) -> Option<Choice> {
        if pressed {
            self.pressed = choice;
            self.hover(choice);
            None
        } else {
            self.pressed
                .take()
                .filter(|previous| Some(*previous) == choice)
        }
    }
}

#[derive(Clone, Copy)]
pub struct DialogGeometry {
    pub body: [f32; 4],
    pub close: [f32; 4],
    pub cancel: [f32; 4],
    pub help: [f32; 4],
}

impl DialogGeometry {
    pub fn new(width: u32, height: u32, scale: f32) -> Self {
        let width = width as f32;
        let height = height as f32;
        let gap = (16.0 * scale).min(width / 12.0);
        let button_width = (180.0 * scale).min((width - 3.0 * gap) / 2.0);
        let button_height = (40.0 * scale).min(height / 6.0);
        let button_top = height * 0.6;
        let left = (width - button_width * 2.0 - gap) / 2.0;
        Self {
            body: [gap, height * 0.15, width - 2.0 * gap, height * 0.4],
            close: [left, button_top, button_width, button_height],
            cancel: [
                left + button_width + gap,
                button_top,
                button_width,
                button_height,
            ],
            help: [
                gap,
                button_top + button_height + gap,
                width - 2.0 * gap,
                height * 0.2,
            ],
        }
    }

    pub fn hit(self, x: f64, y: f64) -> Option<Choice> {
        [(Choice::Close, self.close), (Choice::Cancel, self.cancel)]
            .into_iter()
            .find_map(|(choice, [left, top, width, height])| {
                (x >= f64::from(left)
                    && x < f64::from(left + width)
                    && y >= f64::from(top)
                    && y < f64::from(top + height))
                .then_some(choice)
            })
    }
}

pub fn needs_confirmation(pid: Option<u32>, explicit_command: bool) -> io::Result<bool> {
    let Some(pid) = pid else {
        return Ok(true);
    };
    let root = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => parse_process(&stat),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let Some((state, _)) = root else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid process status",
        ));
    };
    if matches!(state, 'Z' | 'X' | 'x') {
        return Ok(false);
    }
    if explicit_command {
        return Ok(true);
    }
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if !entry
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let stat = match fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if let Some((state, parent)) = parse_process(&stat)
            && parent == pid
            && !matches!(state, 'Z' | 'X' | 'x')
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn shell_was_replaced(pid: Option<u32>, executable: &Path) -> io::Result<bool> {
    let Some(pid) = pid else {
        return Ok(true);
    };
    match fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(current) => Ok(current != executable),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn parse_process(stat: &str) -> Option<(char, u32)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    Some((fields.next()?.chars().next()?, fields.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_select_chinese_scripts_and_fall_back_to_english() {
        for locale in ["zh_CN.UTF-8", "zh-SG", "zh_Hans", "zh_Hans_TW", "zh"] {
            assert_eq!(Language::from_locale(locale), Language::SimplifiedChinese);
        }
        for locale in ["zh_TW.UTF-8", "zh-Hant", "zh_Hant_CN", "zh_HK", "zh_MO"] {
            assert_eq!(Language::from_locale(locale), Language::TraditionalChinese);
        }
        for locale in ["", "C", "en_US.UTF-8", "de_DE", "C.UTF-8"] {
            assert_eq!(Language::from_locale(locale), Language::English);
        }
    }

    #[test]
    fn dialog_defaults_to_cancel_and_supports_keyboard_and_pointer() {
        let mut dialog = CloseDialog::new(Language::English);
        assert_eq!(
            dialog.key(&Key::Named(NamedKey::Enter)),
            Some(Choice::Cancel)
        );
        dialog.key(&Key::Named(NamedKey::Tab));
        assert_eq!(
            dialog.key(&Key::Named(NamedKey::Enter)),
            Some(Choice::Close)
        );
        assert_eq!(
            dialog.key(&Key::Named(NamedKey::Escape)),
            Some(Choice::Cancel)
        );
        assert_eq!(dialog.key(&Key::Character("y".into())), Some(Choice::Close));
        assert_eq!(
            dialog.key(&Key::Character("N".into())),
            Some(Choice::Cancel)
        );
        assert_eq!(dialog.mouse(Some(Choice::Close), true), None);
        assert_eq!(dialog.mouse(Some(Choice::Cancel), false), None);
        dialog.mouse(Some(Choice::Cancel), true);
        assert_eq!(
            dialog.mouse(Some(Choice::Cancel), false),
            Some(Choice::Cancel)
        );
    }

    #[test]
    fn process_names_with_parentheses_do_not_confuse_status_parsing() {
        assert_eq!(
            parse_process("123 (shell (test)) S 42 123 123"),
            Some(('S', 42))
        );
        assert_eq!(parse_process("bad input"), None);
    }

    #[test]
    fn button_hit_testing_tracks_window_dimensions_and_scale() {
        for scale in [1.0, 1.5, 2.0] {
            for (width, height) in [(320, 200), (900, 500), (1800, 1000)] {
                let geometry = DialogGeometry::new(width, height, scale);
                for (choice, [left, top, width, height]) in [
                    (Choice::Close, geometry.close),
                    (Choice::Cancel, geometry.cancel),
                ] {
                    assert_eq!(
                        geometry.hit(f64::from(left + width / 2.0), f64::from(top + height / 2.0)),
                        Some(choice)
                    );
                }
                assert_eq!(geometry.hit(0.0, 0.0), None);
            }
        }
    }
}
