use std::io::{self, Read, Write};

/// The bytes read from a nonblocking stream since its last newline.
#[derive(Default)]
pub struct Lines {
    buffer: Vec<u8>,
}

impl Lines {
    /// Read what `stream` holds, passing each whole line, without its newline,
    /// to `line`. Return false once the stream has closed or failed.
    pub fn read(&mut self, mut stream: impl Read, mut line: impl FnMut(&str)) -> bool {
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

/// The lines written to a nonblocking stream that it has not taken yet.
#[derive(Default)]
pub struct Queue {
    buffer: Vec<u8>,

    /// How many bytes at the front of `buffer` the stream has taken.
    sent: usize,
}

impl Queue {
    /// Add `line` and its newline at the end.
    pub fn push(&mut self, line: &str) {
        self.buffer.extend_from_slice(line.as_bytes());
        self.buffer.push(b'\n');
    }

    pub fn is_empty(&self) -> bool {
        self.sent == self.buffer.len()
    }

    /// Write as much as `stream` takes. Return whether all of it went, or
    /// the error that stopped it, for which the stream is broken.
    pub fn write(&mut self, mut stream: impl Write) -> io::Result<bool> {
        while !self.is_empty() {
            match stream.write(&self.buffer[self.sent..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.sent += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        self.buffer.clear();
        self.sent = 0;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

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

    #[test]
    fn a_line_of_several_megabytes_reaches_a_reader_that_starts_late_whole() {
        let (writer, reader) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        let line = "A".repeat(5 << 20);
        let mut queue = Queue::default();
        queue.push(&line);
        queue.push("after");
        assert!(!queue.write(&writer).unwrap());
        assert!(!queue.is_empty());
        let reading = std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            let (mut first, mut second) = (String::new(), String::new());
            reader.read_line(&mut first).unwrap();
            reader.read_line(&mut second).unwrap();
            (first, second)
        });
        while !queue.write(&writer).unwrap() {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(queue.is_empty());
        let (first, second) = reading.join().unwrap();
        assert_eq!(first.len(), line.len() + 1);
        assert!(first.starts_with(&line));
        assert_eq!(second, "after\n");
    }

    #[test]
    fn a_queue_to_a_closed_stream_fails_rather_than_waiting() {
        let (writer, reader) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        drop(reader);
        let mut queue = Queue::default();
        queue.push("lost");
        assert!(queue.write(&writer).is_err());
    }
}
