//! egui key events -> xterm-compatible byte sequences.
//!
//! Printable text arrives separately as `egui::Event::Text`; this module only handles
//! keys that produce no text event (navigation, function keys, Ctrl/Alt combos).

use egui::{Key, Modifiers};

/// Terminal modes that change how keys are encoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMode {
    /// DECCKM: arrows/Home/End send `ESC O x` instead of `ESC [ x`.
    pub app_cursor: bool,
}

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn xterm_mod(m: Modifiers) -> u8 {
    1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl)
}

fn has_mods(m: Modifiers) -> bool {
    m.shift || m.alt || m.ctrl
}

/// `CSI 1 ; m X` with modifiers, else `SS3 X` (app mode) or `CSI X`.
fn cursor_key(letter: u8, m: Modifiers, mode: KeyMode) -> Vec<u8> {
    if has_mods(m) {
        format!("\x1b[1;{}{}", xterm_mod(m), letter as char).into_bytes()
    } else if mode.app_cursor {
        vec![0x1b, b'O', letter]
    } else {
        vec![0x1b, b'[', letter]
    }
}

/// `CSI n ~` or `CSI n ; m ~`.
fn tilde_key(n: u8, m: Modifiers) -> Vec<u8> {
    if has_mods(m) {
        format!("\x1b[{n};{}~", xterm_mod(m)).into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}

/// F1-F4: `SS3 P..S`, with modifiers `CSI 1 ; m P..S`.
fn ss3_fkey(letter: u8, m: Modifiers) -> Vec<u8> {
    if has_mods(m) {
        format!("\x1b[1;{}{}", xterm_mod(m), letter as char).into_bytes()
    } else {
        vec![0x1b, b'O', letter]
    }
}

fn letter_of(key: Key) -> Option<u8> {
    use Key::*;
    Some(match key {
        A => b'a',
        B => b'b',
        C => b'c',
        D => b'd',
        E => b'e',
        F => b'f',
        G => b'g',
        H => b'h',
        I => b'i',
        J => b'j',
        K => b'k',
        L => b'l',
        M => b'm',
        N => b'n',
        O => b'o',
        P => b'p',
        Q => b'q',
        R => b'r',
        S => b's',
        T => b't',
        U => b'u',
        V => b'v',
        W => b'w',
        X => b'x',
        Y => b'y',
        Z => b'z',
        _ => return None,
    })
}

/// Encode a key press. Returns `None` for keys that are delivered as text instead
/// (plain letters, digits, punctuation, space) or that a terminal ignores.
///
/// `m.ctrl` is the physical Control key on every platform (on macOS `Cmd` is
/// `m.mac_cmd` and is never treated as Control here).
pub fn encode_key(key: Key, m: Modifiers, mode: KeyMode) -> Option<Vec<u8>> {
    use Key::*;
    let alt_prefix = |mut v: Vec<u8>| {
        if m.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    let out = match key {
        ArrowUp => cursor_key(b'A', m, mode),
        ArrowDown => cursor_key(b'B', m, mode),
        ArrowRight => cursor_key(b'C', m, mode),
        ArrowLeft => cursor_key(b'D', m, mode),
        Home => cursor_key(b'H', m, mode),
        End => cursor_key(b'F', m, mode),
        Insert => tilde_key(2, m),
        Delete => tilde_key(3, m),
        PageUp => tilde_key(5, m),
        PageDown => tilde_key(6, m),
        F1 => ss3_fkey(b'P', m),
        F2 => ss3_fkey(b'Q', m),
        F3 => ss3_fkey(b'R', m),
        F4 => ss3_fkey(b'S', m),
        F5 => tilde_key(15, m),
        F6 => tilde_key(17, m),
        F7 => tilde_key(18, m),
        F8 => tilde_key(19, m),
        F9 => tilde_key(20, m),
        F10 => tilde_key(21, m),
        F11 => tilde_key(23, m),
        F12 => tilde_key(24, m),
        Enter => alt_prefix(vec![b'\r']),
        Tab if m.shift => b"\x1b[Z".to_vec(),
        Tab => alt_prefix(vec![b'\t']),
        Escape => alt_prefix(vec![0x1b]),
        Backspace if m.ctrl => alt_prefix(vec![0x08]),
        Backspace => alt_prefix(vec![0x7f]),
        // Ctrl + punctuation (classic C0 control codes).
        Space if m.ctrl => alt_prefix(vec![0x00]),
        OpenBracket if m.ctrl => alt_prefix(vec![0x1b]),
        Backslash if m.ctrl => alt_prefix(vec![0x1c]),
        CloseBracket if m.ctrl => alt_prefix(vec![0x1d]),
        Num6 if m.ctrl && m.shift => alt_prefix(vec![0x1e]), // Ctrl+^
        Minus if m.ctrl && m.shift => alt_prefix(vec![0x1f]), // Ctrl+_
        Slash if m.ctrl => alt_prefix(vec![0x1f]),
        Num2 if m.ctrl => alt_prefix(vec![0x00]), // Ctrl+@
        _ => {
            if m.ctrl {
                // Ctrl+letter -> 0x01..0x1a (Shift does not change it).
                let l = letter_of(key)?;
                alt_prefix(vec![l & 0x1f])
            } else {
                // Plain / Shift / Alt + printable keys come through Event::Text
                // (Alt is turned into an ESC prefix there, see `encode_text`).
                return None;
            }
        }
    };
    Some(out)
}

/// Bytes for pasted text: newlines become CR, and with bracketed paste the text is
/// wrapped in `ESC[200~ … ESC[201~` (embedded ESC removed so the paste cannot end early).
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let mut v = b"\x1b[200~".to_vec();
        v.extend(normalized.replace('\x1b', "").bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        normalized.into_bytes()
    }
}

/// Bytes for a text event. With Alt held (not on macOS, where Option composes
/// characters) the text is prefixed with ESC ("meta sends escape").
pub fn encode_text(text: &str, alt_as_meta: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(text.len() + 1);
    if alt_as_meta {
        v.push(0x1b);
    }
    v.extend_from_slice(text.as_bytes());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: Modifiers = Modifiers::NONE;
    const C: Modifiers = Modifiers::CTRL;
    const S: Modifiers = Modifiers::SHIFT;
    const A: Modifiers = Modifiers::ALT;
    const NORMAL: KeyMode = KeyMode { app_cursor: false };
    const APP: KeyMode = KeyMode { app_cursor: true };

    #[test]
    fn key_table() {
        let cs = C.plus(S);
        let table: &[(Key, Modifiers, KeyMode, Option<&[u8]>)] = &[
            (Key::Enter, N, NORMAL, Some(b"\r")),
            (Key::Enter, A, NORMAL, Some(b"\x1b\r")),
            (Key::Backspace, N, NORMAL, Some(b"\x7f")),
            (Key::Backspace, C, NORMAL, Some(b"\x08")),
            (Key::Backspace, A, NORMAL, Some(b"\x1b\x7f")),
            (Key::Tab, N, NORMAL, Some(b"\t")),
            (Key::Tab, S, NORMAL, Some(b"\x1b[Z")),
            (Key::Escape, N, NORMAL, Some(b"\x1b")),
            (Key::ArrowUp, N, NORMAL, Some(b"\x1b[A")),
            (Key::ArrowDown, N, NORMAL, Some(b"\x1b[B")),
            (Key::ArrowRight, N, NORMAL, Some(b"\x1b[C")),
            (Key::ArrowLeft, N, NORMAL, Some(b"\x1b[D")),
            (Key::ArrowUp, N, APP, Some(b"\x1bOA")),
            (Key::ArrowLeft, N, APP, Some(b"\x1bOD")),
            (Key::ArrowRight, C, NORMAL, Some(b"\x1b[1;5C")),
            (Key::ArrowRight, C, APP, Some(b"\x1b[1;5C")),
            (Key::ArrowUp, S, NORMAL, Some(b"\x1b[1;2A")),
            (Key::ArrowLeft, A, NORMAL, Some(b"\x1b[1;3D")),
            (Key::Home, N, NORMAL, Some(b"\x1b[H")),
            (Key::End, N, NORMAL, Some(b"\x1b[F")),
            (Key::Home, N, APP, Some(b"\x1bOH")),
            (Key::End, N, APP, Some(b"\x1bOF")),
            (Key::Insert, N, NORMAL, Some(b"\x1b[2~")),
            (Key::Delete, N, NORMAL, Some(b"\x1b[3~")),
            (Key::Delete, C, NORMAL, Some(b"\x1b[3;5~")),
            (Key::PageUp, N, NORMAL, Some(b"\x1b[5~")),
            (Key::PageDown, N, NORMAL, Some(b"\x1b[6~")),
            (Key::PageUp, S, NORMAL, Some(b"\x1b[5;2~")),
            (Key::F1, N, NORMAL, Some(b"\x1bOP")),
            (Key::F2, N, NORMAL, Some(b"\x1bOQ")),
            (Key::F3, N, NORMAL, Some(b"\x1bOR")),
            (Key::F4, N, NORMAL, Some(b"\x1bOS")),
            (Key::F1, S, NORMAL, Some(b"\x1b[1;2P")),
            (Key::F5, N, NORMAL, Some(b"\x1b[15~")),
            (Key::F6, N, NORMAL, Some(b"\x1b[17~")),
            (Key::F7, N, NORMAL, Some(b"\x1b[18~")),
            (Key::F8, N, NORMAL, Some(b"\x1b[19~")),
            (Key::F9, N, NORMAL, Some(b"\x1b[20~")),
            (Key::F10, N, NORMAL, Some(b"\x1b[21~")),
            (Key::F11, N, NORMAL, Some(b"\x1b[23~")),
            (Key::F12, N, NORMAL, Some(b"\x1b[24~")),
            (Key::F12, C, NORMAL, Some(b"\x1b[24;5~")),
            (Key::C, C, NORMAL, Some(b"\x03")),
            (Key::D, C, NORMAL, Some(b"\x04")),
            (Key::A, C, NORMAL, Some(b"\x01")),
            (Key::Z, C, NORMAL, Some(b"\x1a")),
            (Key::L, C.plus(A), NORMAL, Some(b"\x1b\x0c")),
            (Key::OpenBracket, C, NORMAL, Some(b"\x1b")),
            (Key::Backslash, C, NORMAL, Some(b"\x1c")),
            (Key::CloseBracket, C, NORMAL, Some(b"\x1d")),
            (Key::Space, C, NORMAL, Some(b"\x00")),
            (Key::Num2, C, NORMAL, Some(b"\x00")),
            (Key::Slash, C, NORMAL, Some(b"\x1f")),
            (Key::Minus, cs, NORMAL, Some(b"\x1f")),
            (Key::Num6, cs, NORMAL, Some(b"\x1e")),
            (Key::X, cs, NORMAL, Some(b"\x18")),
            // Delivered as Event::Text instead:
            (Key::A, N, NORMAL, None),
            (Key::A, S, NORMAL, None),
            (Key::Space, N, NORMAL, None),
            (Key::Num1, N, NORMAL, None),
            (Key::A, A, NORMAL, None),
        ];
        for (key, m, mode, want) in table {
            let got = encode_key(*key, *m, *mode);
            assert_eq!(got.as_deref(), *want, "{key:?} {m:?} {mode:?}");
        }
    }

    #[test]
    fn paste_and_text() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
        assert_eq!(encode_paste("a\r\nb", false), b"a\rb");
        assert_eq!(encode_paste("x\x1by", true), b"\x1b[200~xy\x1b[201~");
        assert_eq!(encode_text("q", true), b"\x1bq");
        assert_eq!(encode_text("é", false), "é".as_bytes());
    }
}
