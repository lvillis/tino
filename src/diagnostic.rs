use std::ffi::OsStr;
use std::path::Path;

pub(crate) fn escape_str(value: &str) -> String {
    value.escape_debug().collect()
}

#[cfg(target_family = "unix")]
pub(crate) fn escape_os(value: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;

    escape_bytes(value.as_bytes())
}

#[cfg(not(target_family = "unix"))]
pub(crate) fn escape_os(value: &OsStr) -> String {
    escape_str(&value.to_string_lossy())
}

pub(crate) fn escape_path(path: &Path) -> String {
    escape_os(path.as_os_str())
}

pub(crate) fn escape_bytes(bytes: &[u8]) -> String {
    let mut escaped = String::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            b'\\' => escaped.push_str("\\\\"),
            b'\'' => escaped.push_str("\\'"),
            b'"' => escaped.push_str("\\\""),
            0x20..=0x7e => escaped.push(char::from(byte)),
            _ => push_hex_byte(&mut escaped, byte),
        }
    }
    escaped
}

fn push_hex_byte(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    out.push_str("\\x");
    out.push(char::from(HEX[usize::from(byte >> 4)]));
    out.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_family = "unix")]
    #[test]
    fn os_diagnostics_preserve_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let value = std::ffi::OsString::from_vec(vec![b'a', 0xff, b'\n']);

        assert_eq!(escape_os(&value), r"a\xff\n");
    }

    #[test]
    fn string_diagnostics_escape_control_bytes() {
        assert_eq!(escape_str("\u{1b}[31m"), r"\u{1b}[31m");
    }
}
