use std::io::{self, BufRead};

/// Longest line kept whole. Sheets are a few KiB; the cap only bounds
/// memory against a bogus multi-gigabyte "line", and anything past it is
/// dropped rather than failing the parse.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Reads one line, dropping anything past [`MAX_LINE_BYTES`] rather than
/// failing the parse.
pub(crate) fn read_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut truncated = false;
    let mut terminated = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if line.is_empty() && !truncated {
                return Ok(None);
            }
            break;
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let length = newline.unwrap_or(buffer.len());
        let remaining = MAX_LINE_BYTES.saturating_sub(line.len());
        let copy_len = length.min(remaining);
        line.extend_from_slice(&buffer[..copy_len]);
        truncated |= copy_len < length;
        reader.consume(length + usize::from(newline.is_some()));
        if newline.is_some() {
            terminated = true;
            break;
        }
    }
    if terminated && line.last() == Some(&b'\r') {
        line.pop();
    }
    match String::from_utf8(line) {
        Ok(line) => Ok(Some(line)),
        // A cut mid-character is the truncation's doing, not the file's.
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid_up_to = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid_up_to);
            Ok(Some(
                String::from_utf8(bytes).expect("prefix up to valid_up_to is valid UTF-8"),
            ))
        }
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
}
