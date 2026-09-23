//! The global shortcut as the Settings window records and draws it. A shortcut is kept
//! as the string `global-hotkey` parses (`Alt+N`, `Ctrl+Shift+Space`), because that
//! is what the settings file has always held.

use gpui::Keystroke;

/// What one keystroke does to a recorder that is listening.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Recorded {
    /// Only modifiers so far; the chord is still being held.
    Waiting,
    /// Escape on its own: stop listening and keep the shortcut there was.
    Cancelled,
    /// Backspace or Delete on its own: no shortcut at all.
    Cleared,
    Bound(String),
    /// A chord that cannot be a global shortcut, and the sentence that says why.
    Refused(&'static str),
}

pub(super) fn record(keystroke: &Keystroke) -> Recorded {
    let held = &keystroke.modifiers;
    let bare = !(held.control || held.alt || held.shift || held.platform);
    match keystroke.key.as_str() {
        "escape" if bare => return Recorded::Cancelled,
        "backspace" | "delete" if bare => return Recorded::Cleared,
        "shift" | "control" | "alt" | "platform" | "function" | "capslock" => {
            return Recorded::Waiting;
        }
        _ => {}
    }
    let Some(key) = key_name(&keystroke.key) else {
        return Recorded::Refused("That key cannot be part of a global shortcut.");
    };
    // Shift alone would take a capital letter from every app on this Mac.
    if !(held.control || held.alt || held.platform) {
        return Recorded::Refused("A global shortcut needs ⌃, ⌥ or ⌘.");
    }
    let mut parts = Vec::new();
    for (on, name) in [
        (held.control, "Ctrl"),
        (held.alt, "Alt"),
        (held.shift, "Shift"),
        (held.platform, "Cmd"),
    ] {
        if on {
            parts.push(name);
        }
    }
    parts.push(key);
    Recorded::Bound(parts.join("+"))
}

/// The name `global-hotkey` reads for a key as GPUI reports it. A shifted digit or
/// symbol is read back to the key it is printed on, which is what the chord binds.
fn key_name(key: &str) -> Option<&'static str> {
    const LETTERS: [&str; 26] = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
        "S", "T", "U", "V", "W", "X", "Y", "Z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    const FUNCTIONS: [&str; 12] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some(LETTERS[(c.to_ascii_uppercase() as u8 - b'A') as usize]);
        }
        if c.is_ascii_digit() {
            return Some(DIGITS[(c as u8 - b'0') as usize]);
        }
        let printed = match c {
            '!' => '1',
            '@' => '2',
            '#' => '3',
            '$' => '4',
            '%' => '5',
            '^' => '6',
            '&' => '7',
            '*' => '8',
            '(' => '9',
            ')' => '0',
            other => other,
        };
        if printed.is_ascii_digit() {
            return Some(DIGITS[(printed as u8 - b'0') as usize]);
        }
        return Some(match printed {
            '`' | '~' => "Backquote",
            '-' | '_' => "Minus",
            '=' | '+' => "Equal",
            '[' | '{' => "BracketLeft",
            ']' | '}' => "BracketRight",
            '\\' | '|' => "Backslash",
            ';' | ':' => "Semicolon",
            '\'' | '"' => "Quote",
            ',' | '<' => "Comma",
            '.' | '>' => "Period",
            '/' | '?' => "Slash",
            _ => return None,
        });
    }
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<usize>().ok())
        && (1..=12).contains(&n)
    {
        return Some(FUNCTIONS[n - 1]);
    }
    Some(match key {
        "space" => "Space",
        "enter" => "Enter",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "escape" => "Escape",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        _ => return None,
    })
}

/// A stored shortcut as the glyphs it is read in: `Alt+N` is `⌥ N`. Anything the
/// glyphs do not cover is shown as written, so a hand-edited file still reads.
pub(super) fn glyphs(shortcut: &str) -> Vec<String> {
    shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            match part.to_ascii_uppercase().as_str() {
                "CTRL" | "CONTROL" => "⌃",
                "ALT" | "OPTION" => "⌥",
                "SHIFT" => "⇧",
                "CMD" | "COMMAND" | "SUPER" | "CMDORCTRL" | "COMMANDORCONTROL" => "⌘",
                "UP" | "ARROWUP" => "↑",
                "DOWN" | "ARROWDOWN" => "↓",
                "LEFT" | "ARROWLEFT" => "←",
                "RIGHT" | "ARROWRIGHT" => "→",
                "ENTER" => "↩",
                "BACKSPACE" => "⌫",
                "DELETE" => "⌦",
                "ESCAPE" | "ESC" => "⎋",
                "TAB" => "⇥",
                "BACKQUOTE" => "`",
                "MINUS" => "-",
                "EQUAL" => "=",
                "BRACKETLEFT" => "[",
                "BRACKETRIGHT" => "]",
                "BACKSLASH" => "\\",
                "SEMICOLON" => ";",
                "QUOTE" => "'",
                "COMMA" => ",",
                "PERIOD" => ".",
                "SLASH" => "/",
                // A letter reads as its capital; a named key (`Space`, `F5`) as written.
                _ => {
                    let key = part.strip_prefix("Key").filter(|key| key.len() == 1);
                    return match key.unwrap_or(part) {
                        single if single.len() == 1 => single.to_uppercase(),
                        named => named.to_owned(),
                    };
                }
            }
            .to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn press(key: &str, modifiers: Modifiers) -> Recorded {
        record(&Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        })
    }

    fn with(control: bool, alt: bool, shift: bool, platform: bool) -> Modifiers {
        Modifiers {
            control,
            alt,
            shift,
            platform,
            function: false,
        }
    }

    #[::core::prelude::v1::test]
    fn a_chord_is_written_the_way_the_settings_file_holds_it() {
        assert_eq!(
            press("n", with(false, true, false, false)),
            Recorded::Bound("Alt+N".into())
        );
        assert_eq!(
            press("space", with(true, false, true, false)),
            Recorded::Bound("Ctrl+Shift+Space".into())
        );
        assert_eq!(
            press("k", with(true, true, true, true)),
            Recorded::Bound("Ctrl+Alt+Shift+Cmd+K".into())
        );
        assert_eq!(
            press("f5", with(false, false, false, true)),
            Recorded::Bound("Cmd+F5".into())
        );
    }

    #[::core::prelude::v1::test]
    fn every_recorded_chord_parses_as_a_global_hotkey() {
        use std::str::FromStr;
        for (key, modifiers) in [
            ("n", with(false, true, false, false)),
            ("1", with(true, false, false, false)),
            ("!", with(true, false, true, false)),
            ("/", with(false, false, false, true)),
            ("?", with(false, true, true, false)),
            ("up", with(true, true, false, false)),
            ("pagedown", with(false, false, false, true)),
            ("f12", with(true, false, false, false)),
        ] {
            let Recorded::Bound(shortcut) = press(key, modifiers) else {
                panic!("{key} was not bound");
            };
            global_hotkey::hotkey::HotKey::from_str(&shortcut)
                .unwrap_or_else(|error| panic!("{shortcut}: {error}"));
        }
    }

    #[::core::prelude::v1::test]
    fn a_shifted_symbol_binds_the_key_it_is_printed_on() {
        assert_eq!(
            press("!", with(true, false, true, false)),
            Recorded::Bound("Ctrl+Shift+1".into())
        );
        assert_eq!(
            press("?", with(false, true, true, false)),
            Recorded::Bound("Alt+Shift+Slash".into())
        );
    }

    #[::core::prelude::v1::test]
    fn bare_escape_cancels_and_bare_backspace_clears() {
        assert_eq!(press("escape", Modifiers::default()), Recorded::Cancelled);
        assert_eq!(press("backspace", Modifiers::default()), Recorded::Cleared);
        assert_eq!(press("delete", Modifiers::default()), Recorded::Cleared);
        // With a modifier, both are keys like any other.
        assert_eq!(
            press("escape", with(true, false, false, false)),
            Recorded::Bound("Ctrl+Escape".into())
        );
    }

    #[::core::prelude::v1::test]
    fn modifiers_alone_wait_and_a_key_without_one_is_refused() {
        assert_eq!(
            press("shift", with(false, false, true, false)),
            Recorded::Waiting
        );
        assert_eq!(
            press("platform", with(false, false, false, true)),
            Recorded::Waiting
        );
        assert!(matches!(
            press("n", Modifiers::default()),
            Recorded::Refused(_)
        ));
        assert!(matches!(
            press("n", with(false, false, true, false)),
            Recorded::Refused(_)
        ));
        assert!(matches!(
            press("volumeup", with(true, false, false, false)),
            Recorded::Refused(_)
        ));
    }

    #[::core::prelude::v1::test]
    fn a_stored_shortcut_reads_as_glyphs() {
        assert_eq!(glyphs("Alt+N"), ["⌥", "N"]);
        assert_eq!(glyphs("Ctrl+Shift+Space"), ["⌃", "⇧", "Space"]);
        assert_eq!(glyphs("alt+n"), ["⌥", "N"]);
        assert_eq!(glyphs("CmdOrCtrl+KeyK"), ["⌘", "K"]);
        assert_eq!(glyphs("Alt+Shift+Slash"), ["⌥", "⇧", "/"]);
        assert!(glyphs("").is_empty());
    }
}
