use std::fs::{self, File};
use std::io::{Error, ErrorKind, Read};
use std::path::Path;

use encoding_rs::{CoderResult, SHIFT_JIS};

// https://github.com/mozilla/rust-code-analysis/blob/master/src/tools.rs#L44
pub(crate) fn read_file_with_eol(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let file_size = fs::metadata(&path).map_or(1024 * 1024, |m| m.len() as usize);
    if file_size <= 3 {
        // this file is very likely almost empty... so nothing to do on it
        return Ok(None);
    }

    let mut file = File::open(path)?;

    let mut start = vec![0; 64.min(file_size)];
    let start = if file.read_exact(&mut start).is_ok() {
        // Skip the bom if one
        if start[..2] == [b'\xFE', b'\xFF'] || start[..2] == [b'\xFF', b'\xFE'] {
            &start[2..]
        } else if start[..3] == [b'\xEF', b'\xBB', b'\xBF'] {
            &start[3..]
        } else {
            &start
        }
    } else {
        return Ok(None);
    };

    // so start contains more or less 64 chars
    let mut head = String::from_utf8_lossy(start).into_owned();
    // The last char could be wrong because we were in the middle of an utf-8 sequence
    head.pop();
    // now check if there is an invalid char
    if head.contains('\u{FFFD}') {
        return Ok(None);
    }

    let mut data = Vec::with_capacity(file_size + 2);
    data.extend_from_slice(start);

    file.read_to_end(&mut data)?;

    remove_blank_lines(&mut data);

    Ok(Some(data))
}

pub(crate) fn encode_to_utf8(buf: &[u8]) -> std::io::Result<String> {
    let mut decoder = SHIFT_JIS.new_decoder();

    let mut buffer_bytes = [0u8; 4096];
    let buffer_str = match std::str::from_utf8_mut(&mut buffer_bytes[..]) {
        Ok(buffer_str) => buffer_str,
        Err(_) => {
            return Err(Error::new(
                ErrorKind::Other,
                "Cannot convert to str the temporary buffer.",
            ))
        }
    };

    let (result, _, _, _) = decoder.decode_to_str(buf, buffer_str, true);

    if let CoderResult::InputEmpty = result {
        Ok(buffer_str.to_owned())
    } else {
        Err(Error::new(
            ErrorKind::Other,
            "Cannot complete the conversion process.",
        ))
    }
}

fn remove_blank_lines(data: &mut Vec<u8>) {
    let count_trailing = data.iter().rev().take_while(|&c| *c == b'\n').count();
    if count_trailing > 0 {
        data.truncate(data.len() - count_trailing + 1);
    } else {
        data.push(b'\n');
    }
}
