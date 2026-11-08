use std::io::{self, Read};
use std::os::unix::net::UnixStream;

/// The bytes read from a nonblocking stream since its last newline.
#[derive(Default)]
pub struct Lines {
    buffer: Vec<u8>,
}

impl Lines {
    /// Read what `stream` holds, passing each whole line, without its newline,
    /// to `line`. Return false once the stream has closed or failed.
    pub fn read(&mut self, mut stream: &UnixStream, mut line: impl FnMut(&str)) -> bool {
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return false,
                Ok(n) => {
                    self.buffer.extend_from_slice(&chunk[..n]);
                    while let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
                        let bytes: Vec<u8> = self.buffer.drain(..=end).collect();
                        line(&String::from_utf8_lossy(&bytes[..end]));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn whole_lines_are_passed_on_and_a_partial_line_waits_for_its_newline() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut lines = Lines::default();
        let mut seen = Vec::new();
        writer.write_all(b"one\ntw").unwrap();
        assert!(lines.read(&reader, |line| seen.push(line.to_string())));
        assert_eq!(seen, ["one"]);
        writer.write_all(b"o\n\nthree").unwrap();
        assert!(lines.read(&reader, |line| seen.push(line.to_string())));
        assert_eq!(seen, ["one", "two", ""]);
        drop(writer);
        assert!(!lines.read(&reader, |line| seen.push(line.to_string())));
        assert_eq!(seen, ["one", "two", ""]);
    }
}
