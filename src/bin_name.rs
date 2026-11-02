//! The name this binary is installed and invoked as.
//!
//! The package stays `workmux` so the fork rebases cleanly on upstream; only the
//! binary target is renamed. Release artifacts, hooks and skills that call back
//! into the binary use [`BIN`], and embedded upstream text goes through
//! [`rebrand!`](crate::rebrand) so its `workmux` invocations call [`BIN`] instead.

pub const BIN: &str = env!("CARGO_BIN_NAME");

const UPSTREAM: &[u8] = b"workmux";

const fn joins(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'@' | b':')
}

/// Whether the command name `workmux` starts at `i`, as opposed to a compound
/// such as `workmux-status`, `.workmux.yaml`, `raine/workmux` or `workmux.dev`.
const fn is_command_at(src: &[u8], i: usize) -> bool {
    let end = i + UPSTREAM.len();
    if end > src.len() {
        return false;
    }
    let mut k = 0;
    while k < UPSTREAM.len() {
        if src[i + k] != UPSTREAM[k] {
            return false;
        }
        k += 1;
    }
    if i > 0 && joins(src[i - 1]) {
        // `/workmux` after a space or quote names the skill as a slash command.
        let slash_command = src[i - 1] == b'/'
            && (i == 1 || matches!(src[i - 2], b' ' | b'\n' | b'"' | b'\'' | b'`' | b'('));
        if !slash_command {
            return false;
        }
    }
    if end < src.len() && joins(src[end]) {
        // A sentence may end on the name; a dotted name may not.
        let sentence_end =
            src[end] == b'.' && (end + 1 == src.len() || !src[end + 1].is_ascii_alphanumeric());
        if !sentence_end {
            return false;
        }
    }
    true
}

pub const fn rebranded_len(src: &str) -> usize {
    let src = src.as_bytes();
    let mut len = src.len();
    let mut i = 0;
    while i < src.len() {
        if is_command_at(src, i) {
            len = len - UPSTREAM.len() + BIN.len();
            i += UPSTREAM.len();
        } else {
            i += 1;
        }
    }
    len
}

pub const fn rebrand<const N: usize>(src: &str) -> [u8; N] {
    let src = src.as_bytes();
    let bin = BIN.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    let mut o = 0;
    while i < src.len() {
        if is_command_at(src, i) {
            let mut k = 0;
            while k < bin.len() {
                out[o + k] = bin[k];
                k += 1;
            }
            o += bin.len();
            i += UPSTREAM.len();
        } else {
            out[o] = src[i];
            o += 1;
            i += 1;
        }
    }
    out
}

/// Rewrite the command name `workmux` in a constant string to [`BIN`] at compile time.
#[macro_export]
macro_rules! rebrand {
    ($src:expr) => {{
        const SRC: &str = $src;
        const BYTES: [u8; $crate::bin_name::rebranded_len(SRC)] = $crate::bin_name::rebrand(SRC);
        const OUT: &str = match ::std::str::from_utf8(&BYTES) {
            Ok(s) => s,
            Err(_) => panic!("rebranded text is not UTF-8"),
        };
        OUT
    }};
}

#[cfg(test)]
mod tests {
    use super::BIN;

    #[test]
    fn rebrand_rewrites_invocations_and_keeps_compound_names() {
        let src = rebrand!(
            "workmux add x; $`workmux set-window-status`; spawn(\"workmux\", a); use workmux.\n\
             \"/workmux add\" \
             .workmux.yaml workmux-status@workmux raine/workmux workmux.dev ~/.config/workmux/ \
             WORKMUX_X workmux_pipe workmux:"
        );
        assert_eq!(
            src,
            format!(
                "{BIN} add x; $`{BIN} set-window-status`; spawn(\"{BIN}\", a); use {BIN}.\n\
                 \"/{BIN} add\" \
                 .workmux.yaml workmux-status@workmux raine/workmux workmux.dev ~/.config/workmux/ \
                 WORKMUX_X workmux_pipe workmux:"
            )
        );
    }
}
