use caseless::Caseless;
use unicode_normalization::UnicodeNormalization;

/// Canonical caseless search keys. Display names and filesystem paths stay verbatim.
pub(crate) fn append_folded(text: &str, out: &mut Vec<u8>) {
    if text.is_ascii() {
        out.extend(text.bytes().map(|byte| byte.to_ascii_lowercase()));
    } else {
        let mut bytes = [0; 4];
        for ch in text.nfd().default_case_fold().nfd() {
            out.extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folded(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        append_folded(text, &mut out);
        out
    }

    #[test]
    fn canonical_case_variants_match() {
        for (a, b) in [
            ("README", "readme"),
            ("ΟΣ", "ος"),
            ("ΟΣ", "οσ"),
            ("Straße", "STRASSE"),
            ("CAFÉ", "cafe\u{301}"),
            ("İ", "i\u{307}"),
            ("ﬃ", "FFI"),
            ("Ꭰ", "ꭰ"),
        ] {
            assert_eq!(folded(a), folded(b), "{a:?} and {b:?}");
        }
        assert_ne!(folded("café"), folded("cafe"));
        assert_ne!(folded("ı"), folded("i"));
    }

    #[test]
    fn folding_is_context_independent_for_substrings() {
        for (name, query) in [("ΟΣ.txt", "ος"), ("ΟσΑ.txt", "ΟΣ"), ("Straße.txt", "SS")] {
            assert!(memchr::memmem::find(&folded(name), &folded(query)).is_some());
        }
    }

    #[test]
    fn appends_without_changing_previous_keys() {
        let mut out = folded("ONE");
        let boundary = out.len();
        append_folded("CAFÉ", &mut out);
        assert_eq!(&out[..boundary], b"one");
        assert_eq!(&out[boundary..], folded("cafe\u{301}"));
    }
}
