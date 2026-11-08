use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use portable_pty::CommandBuilder;

const TERMINAL_NAMES: &[&str] = &["xterm-ghostty", "xterm-256color", "xterm", "vt100"];
const SYSTEM_DIRECTORIES: &[&str] = &[
    "/etc/terminfo",
    "/lib/terminfo",
    "/usr/share/terminfo",
    "/usr/lib/terminfo",
    "/usr/local/share/terminfo",
];

pub fn configure_environment(command: &mut CommandBuilder) {
    let explicit_term = command
        .iter_extra_env_as_str()
        .any(|(name, value)| name == "TERM" && !value.is_empty());
    if !explicit_term {
        command.env("TERM", terminal_name(command));
    }
    command.env("TERM_PROGRAM", "wisp");
    command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    command.env("COLORTERM", "truecolor");
}

pub fn terminal_name(command: &CommandBuilder) -> &'static str {
    let directories = search_directories(command);
    select_terminal(&directories, command.get_env("TERMINFO"))
}

fn select_terminal(directories: &[PathBuf], inline: Option<&std::ffi::OsStr>) -> &'static str {
    TERMINAL_NAMES
        .iter()
        .copied()
        .find(|name| {
            inline_entry_matches(inline, name)
                || directories
                    .iter()
                    .any(|directory| entry_exists(directory, name))
        })
        .unwrap_or("xterm-256color")
}

fn search_directories(command: &CommandBuilder) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut push = |path: PathBuf| {
        if !path.as_os_str().is_empty() && !directories.contains(&path) {
            directories.push(path);
        }
    };
    if let Some(value) = command.get_env("TERMINFO") {
        let inline = value
            .to_str()
            .is_some_and(|value| value.starts_with("b64:") || value.starts_with("hex:"));
        if !inline {
            push(PathBuf::from(value));
        }
    }
    if let Some(home) = command.get_env("HOME").filter(|value| !value.is_empty()) {
        push(Path::new(home).join(".terminfo"));
    }
    if let Some(value) = command.get_env("TERMINFO_DIRS") {
        for path in std::env::split_paths(value) {
            if path.as_os_str().is_empty() {
                for directory in SYSTEM_DIRECTORIES {
                    push(PathBuf::from(directory));
                }
            } else {
                push(path);
            }
        }
    }
    for directory in SYSTEM_DIRECTORIES {
        push(PathBuf::from(directory));
    }
    directories
}

fn entry_exists(directory: &Path, name: &str) -> bool {
    let initial = name.as_bytes()[0];
    [char::from(initial).to_string(), format!("{initial:02x}")]
        .into_iter()
        .any(|prefix| {
            let path = directory.join(prefix).join(name);
            fs::read(path).is_ok_and(|bytes| compiled_entry_matches(&bytes, name))
        })
}

fn inline_entry_matches(value: Option<&std::ffi::OsStr>, name: &str) -> bool {
    let Some(value) = value.and_then(|value| value.to_str()) else {
        return false;
    };
    let decoded = if let Some(encoded) = value.strip_prefix("b64:") {
        STANDARD.decode(encoded).ok()
    } else if let Some(encoded) = value.strip_prefix("hex:") {
        if encoded.len() % 2 != 0 {
            return false;
        }
        encoded
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let digits = std::str::from_utf8(pair).ok()?;
                u8::from_str_radix(digits, 16).ok()
            })
            .collect::<Option<Vec<_>>>()
    } else {
        None
    };
    decoded.is_some_and(|bytes| compiled_entry_matches(&bytes, name))
}

fn compiled_entry_matches(bytes: &[u8], name: &str) -> bool {
    let Some(header) = bytes.get(..12) else {
        return false;
    };
    let little_endian = match u16::from_le_bytes([header[0], header[1]]) {
        0x011a | 0x021e => true,
        0x1a01 | 0x1e02 => false,
        _ => return false,
    };
    let length = if little_endian {
        u16::from_le_bytes([header[2], header[3]])
    } else {
        u16::from_be_bytes([header[2], header[3]])
    };
    bytes
        .get(12..12 + usize::from(length))
        .is_some_and(|names| {
            names
                .strip_suffix(&[0])
                .unwrap_or(names)
                .split(|byte| *byte == b'|')
                .any(|candidate| candidate == name.as_bytes())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn entry(names: &str, magic: u16, big_endian: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [magic, names.len() as u16 + 1, 0, 0, 0, 0] {
            bytes.extend_from_slice(&if big_endian {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            });
        }
        bytes.extend_from_slice(names.as_bytes());
        bytes.push(0);
        bytes
    }

    #[test]
    fn compiled_names_require_valid_headers_and_match_complete_aliases() {
        for magic in [0x011a, 0x021e] {
            for big_endian in [false, true] {
                let bytes = entry("xterm-ghostty|ghostty|Ghostty", magic, big_endian);
                assert!(compiled_entry_matches(&bytes, "xterm-ghostty"));
                assert!(compiled_entry_matches(&bytes, "ghostty"));
                assert!(!compiled_entry_matches(&bytes, "xterm"));
                assert!(!compiled_entry_matches(&bytes[..13], "xterm-ghostty"));
            }
        }
        assert!(!compiled_entry_matches(
            b"not a compiled terminfo entry",
            "xterm-ghostty"
        ));
    }

    #[test]
    fn ncurses_inline_base64_and_hex_descriptions_are_recognized() {
        let bytes = entry("xterm-ghostty|Ghostty", 0x021e, false);
        let encoded = OsString::from(format!("b64:{}", STANDARD.encode(&bytes)));
        assert!(inline_entry_matches(Some(&encoded), "xterm-ghostty"));
        let encoded = OsString::from(format!(
            "hex:{}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        assert!(inline_entry_matches(Some(&encoded), "xterm-ghostty"));
        assert!(!inline_entry_matches(
            Some(std::ffi::OsStr::new("hex:0z")),
            "xterm-ghostty"
        ));
        assert!(!inline_entry_matches(
            Some(std::ffi::OsStr::new("b64:!!!")),
            "xterm-ghostty"
        ));
    }

    #[test]
    fn missing_ghostty_database_falls_back_and_both_entry_layouts_work() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("wisp-terminfo-{}-{nonce}", std::process::id()));
        fs::create_dir_all(directory.join("78")).unwrap();
        fs::write(
            directory.join("78/xterm-256color"),
            entry("xterm-256color|xterm", 0x021e, false),
        )
        .unwrap();
        assert_eq!(
            select_terminal(std::slice::from_ref(&directory), None),
            "xterm-256color"
        );
        fs::create_dir_all(directory.join("x")).unwrap();
        fs::write(directory.join("x/xterm-ghostty"), b"not valid terminfo").unwrap();
        assert_eq!(
            select_terminal(std::slice::from_ref(&directory), None),
            "xterm-256color"
        );
        fs::write(
            directory.join("x/xterm-ghostty"),
            entry("xterm-ghostty|Ghostty", 0x011a, false),
        )
        .unwrap();
        assert_eq!(
            select_terminal(std::slice::from_ref(&directory), None),
            "xterm-ghostty"
        );
        fs::remove_dir_all(directory).unwrap();
        assert_eq!(select_terminal(&[], None), "xterm-256color");
    }

    #[test]
    fn explicit_program_term_is_preserved_and_inherited_term_is_replaced() {
        let mut command = CommandBuilder::new("sh");
        command.env_clear();
        command.env("TERM", "linux");
        configure_environment(&mut command);
        assert_eq!(command.get_env("TERM"), Some(std::ffi::OsStr::new("linux")));
        assert_eq!(
            command.get_env("TERM_PROGRAM"),
            Some(std::ffi::OsStr::new("wisp"))
        );
        assert_eq!(
            command.get_env("TERM_PROGRAM_VERSION"),
            Some(std::ffi::OsStr::new(env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(
            command.get_env("COLORTERM"),
            Some(std::ffi::OsStr::new("truecolor"))
        );
        let mut inherited = CommandBuilder::new("sh");
        let expected = terminal_name(&inherited);
        configure_environment(&mut inherited);
        assert_eq!(
            inherited.get_env("TERM"),
            Some(std::ffi::OsStr::new(expected))
        );
    }
}
