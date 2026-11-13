const WINDOWS_DEVICES: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

pub fn windows_refuses(name: &str) -> bool {
    let device = name
        .split_once('.')
        .map_or(name, |(before, _)| before)
        .trim_end_matches(' ');
    let port = device.get(..3).unwrap_or_default();
    let mut number = device.get(3..).unwrap_or_default().chars();
    let numbered_port = (port.eq_ignore_ascii_case("COM") || port.eq_ignore_ascii_case("LPT"))
        && matches!(
            (number.next(), number.next()),
            (Some('0'..='9' | '¹' | '²' | '³'), None)
        );
    let reserved = WINDOWS_DEVICES
        .iter()
        .any(|reserved| device.eq_ignore_ascii_case(reserved));
    let unwritable = |c: char| c < ' ' || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*');
    numbered_port || reserved || name.ends_with(['.', ' ']) || name.chars().any(unwritable)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Exact,
    Windows,
    MacOs,
}

fn upcased(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (c, upper.next(), upper.next()) {
        ('\u{1f80}'..='\u{1f87}' | '\u{1f90}'..='\u{1f97}' | '\u{1fa0}'..='\u{1fa7}', ..) => {
            char::from_u32(u32::from(c) + 8).unwrap_or(c)
        }
        ('\u{1fb3}' | '\u{1fc3}' | '\u{1ff3}', ..) => char::from_u32(u32::from(c) + 9).unwrap_or(c),
        (_, Some(one), None) if one.to_lowercase().eq([c]) => one,
        _ => c,
    }
}

pub fn folded(name: &str, by: Comparison) -> std::borrow::Cow<'_, str> {
    match by {
        Comparison::Exact => std::borrow::Cow::Borrowed(name),
        Comparison::Windows => name
            .chars()
            .map(|c| if c <= '\u{ffff}' { upcased(c) } else { c })
            .collect(),
        Comparison::MacOs => {
            let nfd = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd();
            let mut caseless = String::new();
            for c in nfd.normalize(name).chars() {
                match c {
                    'ı' => caseless.push(c),
                    _ => caseless.extend(
                        c.to_lowercase()
                            .flat_map(char::to_uppercase)
                            .flat_map(char::to_lowercase),
                    ),
                }
            }
            std::borrow::Cow::Owned(nfd.normalize(&caseless).into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_windows_would_rewrite_or_send_to_a_device_are_refused() {
        for name in [
            "12:30.log",
            "a:b",
            "notes.",
            "notes ",
            "notes. .",
            "nul",
            "NUL",
            "nul.txt",
            "NUL .txt",
            "aux.tar.gz",
            "con",
            "Prn",
            "CONIN$",
            "conout$.log",
            "com0",
            "COM1",
            "com9.txt",
            "lpt0",
            "LPT9",
            "com¹",
            "LPT²",
            "lpt³.log",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "tab\there",
            "\u{1}x",
            "line\nbreak",
        ] {
            assert!(windows_refuses(name), "{name:?} was let through");
        }
    }

    #[test]
    fn names_that_only_look_like_devices_pass() {
        for name in [
            "notes",
            "notes.txt",
            ".hidden",
            "a.b.c",
            "com",
            "lpt",
            "com10",
            "lpt1x",
            "com⁴",
            "console",
            "nul_x",
            "auxiliary.txt",
            "connect.log",
            "звіт.txt",
            "日本語",
            "a\u{7f}b",
        ] {
            assert!(!windows_refuses(name), "{name:?} was refused");
        }
    }

    fn same(a: &str, b: &str, by: Comparison) -> bool {
        folded(a, by) == folded(b, by)
    }

    #[test]
    fn names_that_differ_only_in_case_are_one_name_on_windows_and_macos() {
        for (a, b) in [
            ("README.txt", "readme.txt"),
            ("Notes.txt", "NOTES.TXT"),
            ("Звіт.TXT", "звіт.txt"),
            ("ΣΠΊΤΙ", "σπίτι"),
        ] {
            assert!(same(a, b, Comparison::Windows), "{a} and {b} on Windows");
            assert!(same(a, b, Comparison::MacOs), "{a} and {b} on macOS");
            assert!(!same(a, b, Comparison::Exact), "{a} and {b} on Linux");
        }
    }

    #[test]
    fn only_macos_takes_composed_and_decomposed_letters_as_one() {
        let (composed, decomposed) = ("caf\u{e9}.txt", "cafe\u{301}.txt");
        assert!(same(composed, decomposed, Comparison::MacOs));
        assert!(same("CAFE\u{301}.TXT", composed, Comparison::MacOs));
        assert!(!same(composed, decomposed, Comparison::Windows));
        assert!(!same(composed, decomposed, Comparison::Exact));
    }

    #[test]
    fn names_that_differ_in_more_than_case_stay_apart() {
        for (a, b) in [
            ("a.txt", "b.txt"),
            ("notes", "notes.txt"),
            ("notes.txt", "notes.txt "),
            ("cafe.txt", "caf\u{e9}.txt"),
        ] {
            for by in [Comparison::Exact, Comparison::Windows, Comparison::MacOs] {
                assert!(!same(a, b, by), "{a} and {b} under {by:?}");
            }
        }
    }

    #[test]
    fn names_ntfs_keeps_apart_stay_apart_on_windows() {
        for (a, b) in [
            ("straße", "STRASSE"),
            ("\u{10428}", "\u{10400}"),
            ("ſ", "s"),
            ("\u{b5}", "\u{3bc}"),
            ("ς", "σ"),
        ] {
            assert!(!same(a, b, Comparison::Windows), "{a} and {b}");
        }
    }

    #[test]
    fn dotless_i_is_a_letter_of_its_own_on_windows_and_macos() {
        for by in [Comparison::Windows, Comparison::MacOs] {
            assert!(!same("dış.jpg", "diş.jpg", by), "{by:?}");
            assert!(!same("dış.jpg", "DIŞ.jpg", by), "{by:?}");
            assert!(same("diş.jpg", "DIŞ.JPG", by), "{by:?}");
        }
    }

    #[test]
    fn macos_folds_case_fully_as_apfs_does() {
        for (a, b) in [
            ("Maße.pdf", "MASSE.pdf"),
            ("STRAẞE", "straße"),
            ("ﬁle.txt", "FILE.txt"),
            ("ſ", "s"),
            ("\u{b5}", "\u{3bc}"),
            ("ς", "σ"),
        ] {
            assert!(same(a, b, Comparison::MacOs), "{a} and {b}");
        }
    }

    #[test]
    fn a_letter_with_iota_subscript_and_its_titlecase_are_one_name() {
        for (a, b) in [
            ("\u{1f80}", "\u{1f88}"),
            ("\u{1fa7}", "\u{1faf}"),
            ("\u{1fb3}", "\u{1fbc}"),
            ("\u{1ff3}", "\u{1ffc}"),
        ] {
            assert!(same(a, b, Comparison::Windows), "{a} and {b} on Windows");
            assert!(same(a, b, Comparison::MacOs), "{a} and {b} on macOS");
        }
    }
}
