//! Magic-number vocabulary: what counts as a magic value at all.

/// Magic numbers: a numeric literal is magic when its text is outside the
/// (0, 1, 2) trivial set. The position rule (operand of an operation, not a
/// keyword value / definition site) is language-layer logic.
pub fn is_magic_value(text: &str) -> bool {
    !matches!(text, "0" | "1" | "2")
}

// --------------------------------------------------------------------------- duplicates
