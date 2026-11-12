#[cfg(target_os = "linux")]
use std::fs;
use std::io;
use std::path::Path;

#[cfg(windows)]
use std::os::windows::ffi::OsStringExt;
#[cfg(windows)]
use std::path::PathBuf;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use winit::keyboard::{Key, NamedKey};

use crate::config::Config;
use crate::terminal::{CellSnapshot, CursorSnapshot, Rgb, TerminalSnapshot};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Choice {
    Close,
    Cancel,
    Sanitize,
    PasteAnyway,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogKind {
    Close,
    Paste,
}

#[derive(Clone, Debug)]
pub struct CloseDialog {
    pub kind: DialogKind,
    pub selected: Choice,
    pub message: String,
    pressed: Option<Choice>,
}

#[derive(Clone, Copy)]
struct Button {
    choice: Choice,
    label: &'static str,
    shortcut: char,
    color: usize,
}

const CLOSE_BUTTONS: [Button; 2] = [
    Button {
        choice: Choice::Close,
        label: "Yes",
        shortcut: 'y',
        color: 2,
    },
    Button {
        choice: Choice::Cancel,
        label: "No",
        shortcut: 'n',
        color: 1,
    },
];
const PASTE_BUTTONS: [Button; 3] = [
    Button {
        choice: Choice::Sanitize,
        label: "Sanitize and paste",
        shortcut: 's',
        color: 2,
    },
    Button {
        choice: Choice::PasteAnyway,
        label: "Paste anyway",
        shortcut: 'a',
        color: 1,
    },
    Button {
        choice: Choice::Cancel,
        label: "Cancel",
        shortcut: 'c',
        color: 3,
    },
];

impl CloseDialog {
    pub fn new(message: String) -> Self {
        Self {
            kind: DialogKind::Close,
            selected: Choice::Close,
            message,
            pressed: None,
        }
    }

    pub fn paste() -> Self {
        Self {
            kind: DialogKind::Paste,
            selected: Choice::Sanitize,
            message: "The text to be pasted contains terminal control codes.\n\nIf the terminal program you are pasting into does not properly sanitize text, this can lead to code execution vulnerabilities.\n\nHow would you like to proceed?".to_owned(),
            pressed: None,
        }
    }

    fn buttons(&self) -> &[Button] {
        match self.kind {
            DialogKind::Close => &CLOSE_BUTTONS,
            DialogKind::Paste => &PASTE_BUTTONS,
        }
    }

    pub fn key(&mut self, key: &Key) -> Option<Choice> {
        match key {
            Key::Named(NamedKey::Escape) => Some(Choice::Cancel),
            Key::Named(NamedKey::Enter) => Some(self.selected),
            Key::Character(text) => {
                let shortcut = text.to_ascii_lowercase();
                self.buttons()
                    .iter()
                    .find(|button| shortcut == button.shortcut.to_string())
                    .map(|button| button.choice)
                    .or_else(|| (self.kind == DialogKind::Close).then_some(Choice::Cancel))
            }
            _ => None,
        }
    }

    pub fn mouse(&mut self, choice: Option<Choice>, pressed: bool) -> Option<Choice> {
        if pressed {
            self.pressed = choice;
            None
        } else {
            self.pressed
                .take()
                .filter(|previous| Some(*previous) == choice)
        }
    }

    pub fn layout(&self, columns: u16, rows: u16, config: &Config) -> DialogLayout {
        let columns = usize::from(columns.max(1));
        let rows = usize::from(rows.max(1));
        let width = columns.saturating_sub(2);
        let lines = wrap_message(&self.message, width);
        let mut layout = DialogLayout {
            frame: TerminalSnapshot {
                cols: columns as u16,
                rows: rows as u16,
                cells: vec![
                    CellSnapshot {
                        foreground: config.colors.foreground.into(),
                        ..Default::default()
                    };
                    columns * rows
                ],
                background: config.colors.background.into(),
                foreground: config.colors.foreground.into(),
                cursor_color: config.colors.cursor.into(),
                cursor: CursorSnapshot {
                    visible: false,
                    ..Default::default()
                },
                mouse_reporting: false,
                graphics: Default::default(),
            },
            buttons: Vec::new(),
        };
        let mut row = (rows.saturating_sub(lines.len()) / 2).saturating_sub(2);
        let mut message_offset = 0;
        for line in lines {
            let mut column = extra_for(line.width(), width);
            let mut search_offset = message_offset;
            for character in line.chars() {
                if let Some(offset) = self.message[search_offset..].find(character) {
                    search_offset += offset;
                }
                let color = if self.kind == DialogKind::Paste {
                    let danger = self.message.find("code execution vulnerabilities").unwrap();
                    (danger..danger + "code execution vulnerabilities".len())
                        .contains(&search_offset)
                        .then_some(config.colors.color1.into())
                } else {
                    self.message.find("It is running: ").and_then(|start| {
                        let start = start + "It is running: ".len();
                        (start..self.message.rfind('.').unwrap_or(self.message.len()))
                            .contains(&search_offset)
                            .then_some(config.colors.color2.into())
                    })
                };
                search_offset += character.len_utf8();
                layout.put(column, row, character, color, true);
                column += character.width().unwrap_or(0);
            }
            message_offset = search_offset;
            row += 1;
        }
        if rows > 2 {
            row += 1;
        }
        let boxed = row + 3 <= rows;
        if !boxed {
            let mut groups: Vec<Vec<Button>> = vec![Vec::new()];
            let mut used = 0;
            for button in self.buttons() {
                let length = button.label.width() + 2;
                if self.kind == DialogKind::Paste && used + length >= width {
                    groups.push(Vec::new());
                    used = 0;
                }
                groups.last_mut().unwrap().push(*button);
                used += length;
            }
            for group in groups {
                let total = if self.kind == DialogKind::Close {
                    3 + 3 + 2
                } else {
                    group.iter().map(|button| button.label.width() + 2).sum()
                };
                let mut column = extra_for(total, width);
                for button in group {
                    let left = column;
                    let mut highlighted = false;
                    for character in button.label.chars() {
                        let shortcut =
                            !highlighted && character.to_ascii_lowercase() == button.shortcut;
                        highlighted |= shortcut;
                        let color = shortcut.then(|| {
                            config.colors.ansi_palette()[if self.kind == DialogKind::Close
                                && button.choice == Choice::Cancel
                            {
                                9
                            } else {
                                button.color
                            }]
                            .into()
                        });
                        layout.put(column, row, character, color, !self.message.is_empty());
                        if shortcut
                            && self.kind == DialogKind::Paste
                            && button.choice == self.selected
                            && column < columns
                            && row < rows
                        {
                            let cell = &mut layout.frame.cells[row * columns + column];
                            cell.underline = true;
                            cell.underline_style = crate::terminal::UnderlineStyle::Single;
                        }
                        column += character.width().unwrap_or(0);
                    }
                    layout
                        .buttons
                        .push((button.choice, [left, row, button.label.width(), 1]));
                    column += if self.kind == DialogKind::Close { 3 } else { 2 };
                }
                row += 1;
            }
            return layout;
        }
        let mut groups: Vec<Vec<Button>> = vec![Vec::new()];
        let mut used = 0;
        for button in self.buttons() {
            let length = button.label.width() + if boxed { 6 } else { 2 };
            if used + length > width {
                groups.push(Vec::new());
                used = 0;
            }
            groups.last_mut().unwrap().push(*button);
            used += length;
        }
        for group in groups {
            let gap = if boxed {
                2
            } else if self.kind == DialogKind::Close {
                3
            } else {
                2
            };
            let total = group
                .iter()
                .map(|button| button.label.width() + if boxed { 4 } else { 0 })
                .sum::<usize>()
                + group.len().saturating_sub(1) * gap;
            let mut left = extra_for(total, width);
            for button in group {
                let length = button.label.width() + if boxed { 4 } else { 0 };
                let frame_color =
                    (button.choice == self.selected).then_some(config.colors.color11.into());
                let bold = !self.message.is_empty();
                if boxed {
                    layout.put(left, row, '╭', frame_color, bold);
                    layout.put(left + length - 1, row, '╮', frame_color, bold);
                    layout.put(left, row + 2, '╰', frame_color, bold);
                    layout.put(left + length - 1, row + 2, '╯', frame_color, bold);
                    for column in left + 1..left + length - 1 {
                        layout.put(column, row, '─', frame_color, bold);
                        layout.put(column, row + 2, '─', frame_color, bold);
                    }
                    layout.put(left, row + 1, '│', frame_color, bold);
                    layout.put(left + length - 1, row + 1, '│', frame_color, bold);
                }
                let mut column = left + if boxed { 2 } else { 0 };
                let mut highlighted = false;
                for character in button.label.chars() {
                    let shortcut =
                        !highlighted && character.to_ascii_lowercase() == button.shortcut;
                    highlighted |= shortcut;
                    layout.put(
                        column,
                        row + usize::from(boxed),
                        character,
                        shortcut.then(|| config.colors.ansi_palette()[button.color].into()),
                        bold,
                    );
                    column += character.width().unwrap_or(0);
                }
                layout.buttons.push((
                    button.choice,
                    [left, row, length, if boxed { 3 } else { 1 }],
                ));
                left += length + gap;
            }
            row += if boxed { 3 } else { 1 };
        }
        layout
    }
}

pub struct DialogLayout {
    pub frame: TerminalSnapshot,
    pub buttons: Vec<(Choice, [usize; 4])>,
}

impl DialogLayout {
    fn put(&mut self, column: usize, row: usize, character: char, color: Option<Rgb>, bold: bool) {
        if column >= usize::from(self.frame.cols) || row >= usize::from(self.frame.rows) {
            return;
        }
        let index = row * usize::from(self.frame.cols) + column;
        self.frame.cells[index].text = character.to_string();
        self.frame.cells[index].foreground = color.unwrap_or(self.frame.foreground);
        self.frame.cells[index].bold = bold;
        let width = character.width().unwrap_or(0).max(1);
        self.frame.cells[index].width = width as u8;
        if width == 2 && column + 1 < usize::from(self.frame.cols) {
            self.frame.cells[index + 1].width = 0;
        }
    }

    pub fn hit(&self, column: usize, row: usize) -> Option<Choice> {
        self.buttons
            .iter()
            .find_map(|(choice, [left, top, width, height])| {
                (column >= *left && column < left + width && row >= *top && row < top + height)
                    .then_some(*choice)
            })
    }
}

fn extra_for(width: usize, available: usize) -> usize {
    available.saturating_sub(width) / 2 + 1
}

fn wrap_message(message: &str, width: usize) -> Vec<String> {
    if width < 1 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for source in message.split('\n') {
        let mut remaining = source;
        if remaining.is_empty() {
            lines.push(String::new());
        }
        while !remaining.is_empty() {
            let mut used = 0;
            let mut end = remaining.len();
            for (offset, character) in remaining.char_indices() {
                used += character.width().unwrap_or(0);
                if used > width {
                    end = offset;
                    break;
                }
            }
            if end < remaining.len()
                && let Some(space) = remaining[..end].rfind(' ')
                && remaining[..end].width() - space < 12
                && space > 0
            {
                end = space + 1;
            }
            if end == 0 {
                end = remaining.chars().next().unwrap().len_utf8();
            }
            lines.push(remaining[..end].trim().to_owned());
            remaining = &remaining[end..];
        }
    }
    lines
}

pub fn sanitized_paste(text: &str, bracketed: bool) -> Option<String> {
    let mut changed = false;
    let mut sanitized = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '\x00'..='\x08' | '\x0b'..='\x1f' | '\x7f') {
            sanitized.push(char::from_u32(u32::from(character) + 0x2400).unwrap());
            changed = true;
        } else if character == '\n' && !bracketed {
            sanitized.push_str("\x1bE");
            changed = true;
        } else {
            sanitized.push(character);
        }
    }
    changed.then_some(sanitized)
}

pub fn needs_confirmation(pid: Option<u32>, explicit_command: bool) -> io::Result<bool> {
    needs_confirmation_for_shell(pid, explicit_command, None)
}

pub fn needs_confirmation_for_shell(
    pid: Option<u32>,
    explicit_command: bool,
    shell: Option<&std::ffi::OsStr>,
) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    return needs_confirmation_linux(pid, explicit_command);
    #[cfg(windows)]
    return needs_confirmation_windows(pid, explicit_command, shell);
    #[cfg(not(any(target_os = "linux", windows)))]
    return Ok(pid.is_some() && explicit_command);
}

#[cfg(target_os = "linux")]
fn needs_confirmation_linux(pid: Option<u32>, explicit_command: bool) -> io::Result<bool> {
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

#[cfg(windows)]
fn needs_confirmation_windows(
    pid: Option<u32>,
    explicit_command: bool,
    shell: Option<&std::ffi::OsStr>,
) -> io::Result<bool> {
    let Some(pid) = pid else {
        return Ok(true);
    };
    if explicit_command {
        let processes = windows_processes()?;
        if !processes.iter().any(|process| process.pid == pid) {
            return Ok(false);
        }
        return Ok(true);
    }
    let processes = windows_processes()?;
    if !processes.iter().any(|process| process.pid == pid) {
        return Ok(false);
    }
    let root = effective_shell_root(&processes, pid, shell);
    Ok(!descendants(&processes, root).is_empty())
}

#[cfg(target_os = "linux")]
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

#[cfg(windows)]
pub fn shell_was_replaced(pid: Option<u32>, executable: &Path) -> io::Result<bool> {
    let Some(pid) = pid else {
        return Ok(true);
    };
    let Some(current) = process_executable(pid)? else {
        return Ok(false);
    };
    Ok(normalized_windows_path(&current) != normalized_windows_path(executable))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn shell_was_replaced(_pid: Option<u32>, _executable: &Path) -> io::Result<bool> {
    Ok(false)
}

pub fn close_message(pid: Option<u32>) -> String {
    close_message_for_shell(pid, None)
}

pub fn close_message_for_shell(pid: Option<u32>, shell: Option<&std::ffi::OsStr>) -> String {
    #[cfg(target_os = "linux")]
    return close_message_linux(pid);
    #[cfg(windows)]
    return close_message_windows(pid, shell);
    #[cfg(not(any(target_os = "linux", windows)))]
    return "Are you sure you want to close this window? It is running: a program.".to_owned();
}

#[cfg(target_os = "linux")]
fn close_message_linux(pid: Option<u32>) -> String {
    let program = pid
        .and_then(|pid| {
            let children = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
            let child = children
                .split_whitespace()
                .next()
                .and_then(|child| child.parse::<u32>().ok())
                .unwrap_or(pid);
            let command = fs::read(format!("/proc/{child}/cmdline")).ok()?;
            Some(
                String::from_utf8_lossy(command.split(|byte| *byte == 0).next()?)
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect::<String>(),
            )
        })
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| "a program".to_owned());
    format!("Are you sure you want to close this window? It is running: {program}.")
}

#[cfg(windows)]
fn close_message_windows(pid: Option<u32>, shell: Option<&std::ffi::OsStr>) -> String {
    let program = pid
        .and_then(|pid| {
            let processes = windows_processes().ok()?;
            let root = effective_shell_root(&processes, pid, shell);
            let process = descendants(&processes, root)
                .into_iter()
                .next()
                .or_else(|| processes.into_iter().find(|process| process.pid == pid))?;
            Some(process.executable.to_string_lossy().into_owned())
        })
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| "a program".to_owned());
    format!("Are you sure you want to close this window? It is running: {program}.")
}

#[cfg(windows)]
#[derive(Clone)]
struct WindowsProcess {
    pid: u32,
    parent: u32,
    executable: std::ffi::OsString,
}

#[cfg(windows)]
struct OwnedHandle(HANDLE);

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn windows_processes() -> io::Result<Vec<WindowsProcess>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
        return Ok(Vec::new());
    }
    let mut processes = Vec::new();
    loop {
        let length = entry
            .szExeFile
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(entry.szExeFile.len());
        processes.push(WindowsProcess {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            executable: std::ffi::OsString::from_wide(&entry.szExeFile[..length]),
        });
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
            break;
        }
    }
    Ok(processes)
}

#[cfg(windows)]
fn descendants(processes: &[WindowsProcess], pid: u32) -> Vec<WindowsProcess> {
    let mut result: Vec<WindowsProcess> = Vec::new();
    let mut parents = vec![pid];
    while let Some(parent) = parents.pop() {
        for process in processes.iter().filter(|process| process.parent == parent) {
            if process.pid == pid || result.iter().any(|known| known.pid == process.pid) {
                continue;
            }
            parents.push(process.pid);
            result.push(process.clone());
        }
    }
    result
}

#[cfg(windows)]
fn effective_shell_root(
    processes: &[WindowsProcess],
    pid: u32,
    shell: Option<&std::ffi::OsStr>,
) -> u32 {
    let Some(shell_name) = shell.and_then(program_name) else {
        return pid;
    };
    let mut root = pid;
    for _ in 0..8 {
        let mut children = processes.iter().filter(|process| process.parent == root);
        let Some(child) = children.next() else {
            break;
        };
        if children.next().is_some()
            || program_name(&child.executable).as_deref() != Some(&shell_name)
        {
            break;
        }
        root = child.pid;
    }
    root
}

#[cfg(windows)]
fn program_name(program: &std::ffi::OsStr) -> Option<String> {
    Path::new(program)
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase)
}

#[cfg(windows)]
fn process_executable(pid: u32) -> io::Result<Option<PathBuf>> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(87) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let handle = OwnedHandle(handle);
    let mut buffer = vec![0_u16; 32 * 1024];
    let mut length = buffer.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            buffer.as_mut_ptr(),
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(length as usize);
    Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer))))
}

#[cfg(windows)]
fn normalized_windows_path(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = canonical.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or_else(|| text.into_owned());
    text.replace('/', r"\").to_lowercase()
}

#[cfg(target_os = "linux")]
fn parse_process(stat: &str) -> Option<(char, u32)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    Some((fields.next()?.chars().next()?, fields.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_yesno_uses_three_cell_boxes_and_default_yes() {
        let mut dialog = CloseDialog::new("Close?".to_owned());
        assert_eq!(
            dialog.key(&Key::Named(NamedKey::Enter)),
            Some(Choice::Close)
        );
        let layout = dialog.layout(80, 24, &Config::default());
        assert_eq!(
            layout.buttons,
            vec![
                (Choice::Close, [32, 11, 7, 3]),
                (Choice::Cancel, [41, 11, 6, 3])
            ]
        );
        assert_eq!(layout.frame.cells[11 * 80 + 32].text, "╭");
        assert_eq!(layout.hit(33, 12), Some(Choice::Close));
        assert_eq!(
            dialog.key(&Key::Character("x".into())),
            Some(Choice::Cancel)
        );
    }

    #[test]
    fn paste_policy_matches_kitty_controls_and_bracketed_newlines() {
        assert_eq!(sanitized_paste("plain\ttext", false), None);
        assert_eq!(sanitized_paste("one\ntwo", true), None);
        assert_eq!(
            sanitized_paste("one\ntwo", false),
            Some("one\x1bEtwo".to_owned())
        );
        assert_eq!(
            sanitized_paste("\x03\r\x1b[201~", true),
            Some("␃␍␛[201~".to_owned())
        );
        assert_eq!(sanitized_paste("\x7f", false), Some("\u{247f}".to_owned()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_names_with_parentheses_do_not_confuse_status_parsing() {
        assert_eq!(
            parse_process("123 (shell (test)) S 42 123 123"),
            Some(('S', 42))
        );
        assert_eq!(parse_process("bad input"), None);
    }

    #[cfg(windows)]
    #[test]
    fn wrapper_shell_chain_collapses_to_the_real_shell_process() {
        let processes = vec![
            WindowsProcess {
                pid: 10,
                parent: 1,
                executable: "nu.exe".into(),
            },
            WindowsProcess {
                pid: 20,
                parent: 10,
                executable: "nu.exe".into(),
            },
            WindowsProcess {
                pid: 30,
                parent: 20,
                executable: "ping.exe".into(),
            },
        ];
        assert_eq!(
            effective_shell_root(&processes, 10, Some(std::ffi::OsStr::new("nu"))),
            20
        );
        assert_eq!(
            descendants(&processes, 20)
                .into_iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            [30]
        );
    }
}
