use std::path::Path;

use decade_channel::{Channel, Format, Message, Record, Recorded};

use crate::files;

/// Ask decade to record pane `pane` into the new file `file`, with the
/// settings in `flags`, and print what it wrote.
pub fn run(pane: &str, file: &str, flags: &[String]) -> Result<(), String> {
    let mut record = request(pane, flags)?;
    let path = std::path::absolute(file).map_err(|error| format!("{file}: {error}"))?;
    check_file(&path, record.format)?;
    record.file = path.display().to_string();
    record.caller = std::env::var("DECADE_PANE").ok();
    let socket = std::env::var_os("DECADE_SOCKET")
        .ok_or("DECADE_SOCKET is not set; run decade record in a program decade started")?;
    let (mut channel, messages) =
        Channel::connect(Path::new(&socket), "decade record", &[], &[], None, None)
            .map_err(|error| format!("connecting to decade failed: {error}"))?;
    channel
        .record(&record)
        .map_err(|error| format!("asking decade for a recording failed: {error}"))?;
    let recorded = loop {
        match messages.recv() {
            Ok(Message::Record(recorded)) => break recorded,
            Ok(Message::Error { reason, .. }) => return Err(reason),
            Ok(Message::Closed) | Err(_) => {
                return Err("decade closed the connection before the recording ended".to_string());
            }
            Ok(_) => {}
        }
    };
    println!("{}", summary(&recorded));
    Ok(())
}

/// The line the command prints for a finished recording.
pub fn summary(recorded: &Recorded) -> String {
    let Recorded {
        file,
        frames,
        dropped,
        seconds,
        ended,
    } = recorded;
    format!("recorded {frames} frames over {seconds:.1} s to {file}, {dropped} dropped; {ended}")
}

/// The `record` message for pane `pane` and the command line's `flags`,
/// without its file.
fn request(pane: &str, flags: &[String]) -> Result<Record, String> {
    let mut record = Record {
        pane: pane.to_string(),
        ..Record::default()
    };
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let mut value = || {
            flags
                .next()
                .ok_or_else(|| format!("{flag} needs a value"))
                .cloned()
        };
        match flag.as_str() {
            "--format" => {
                record.format = Some(match value()?.as_str() {
                    "mp4" => Format::Mp4,
                    "gif" => Format::Gif,
                    other => return Err(format!("--format is mp4 or gif, not {other}")),
                })
            }
            "--fps" => record.fps = Some(number(flag, &value()?)?),
            "--duration" => record.duration = Some(number(flag, &value()?)?),
            "--width" => record.width = Some(number(flag, &value()?)?),
            "--quality" => record.quality = Some(number(flag, &value()?)?),
            "--no-pointer" => record.pointer = Some(false),
            "--once" => record.r#loop = Some(false),
            other => return Err(format!("no flag {other}")),
        }
    }
    Ok(record)
}

fn number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} takes a number, not {value}"))
}

/// The format of a recording into `file`: `format`, which must match the
/// file's extension, or else the extension's.
pub fn format_for(file: &Path, format: Option<Format>) -> Result<Format, String> {
    let shown = file.display();
    let extension = match file.extension().and_then(|extension| extension.to_str()) {
        Some("mp4") => Format::Mp4,
        Some("gif") => Format::Gif,
        _ => return Err(format!("{shown} does not end in .mp4 or .gif")),
    };
    match format {
        Some(format) if format != extension => Err(format!(
            "{shown} does not end in .{}, as format {} needs",
            name(format),
            name(format)
        )),
        _ => Ok(extension),
    }
}

fn name(format: Format) -> &'static str {
    match format {
        Format::Mp4 => "mp4",
        Format::Gif => "gif",
    }
}

/// Refuse `file` unless its extension fits `format`, its directory exists
/// and nothing is at its path.
fn check_file(file: &Path, format: Option<Format>) -> Result<(), String> {
    format_for(file, format)?;
    files::check_new(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_flags_fill_the_record_message_and_absent_ones_stay_unset() {
        let record = request(
            "code",
            &flags("--format gif --fps 12 --duration 2.5 --width 640 --no-pointer --once"),
        )
        .unwrap();
        assert_eq!(
            record,
            Record {
                pane: "code".to_string(),
                format: Some(Format::Gif),
                fps: Some(12),
                duration: Some(2.5),
                width: Some(640),
                pointer: Some(false),
                r#loop: Some(false),
                ..Record::default()
            }
        );
        assert_eq!(
            request("code", &flags("--quality 18")).unwrap().quality,
            Some(18)
        );
        assert_eq!(request("code", &[]).unwrap().fps, None);
    }

    #[test]
    fn a_bad_flag_or_value_is_refused() {
        let refused = |text: &str| request("code", &flags(text)).unwrap_err();
        assert_eq!(refused("--format webm"), "--format is mp4 or gif, not webm");
        assert_eq!(refused("--fps fast"), "--fps takes a number, not fast");
        assert_eq!(refused("--fps"), "--fps needs a value");
        assert_eq!(refused("--loud"), "no flag --loud");
    }

    #[test]
    fn the_format_comes_from_the_extension_and_must_agree_with_one_given() {
        let gif = Path::new("/work/demo.gif");
        assert_eq!(format_for(gif, None), Ok(Format::Gif));
        assert_eq!(format_for(gif, Some(Format::Gif)), Ok(Format::Gif));
        assert_eq!(
            format_for(gif, Some(Format::Mp4)),
            Err("/work/demo.gif does not end in .mp4, as format mp4 needs".to_string())
        );
        assert_eq!(
            format_for(Path::new("/work/demo.mp4"), None),
            Ok(Format::Mp4)
        );
        for file in ["/work/demo.webm", "/work/demo", "/work/.gif"] {
            assert_eq!(
                format_for(Path::new(file), None),
                Err(format!("{file} does not end in .mp4 or .gif"))
            );
        }
    }

    #[test]
    fn a_file_that_exists_or_whose_directory_is_missing_is_refused() {
        let dir = std::env::temp_dir().join(format!("decade-record-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("demo.gif");
        let _ = std::fs::remove_file(&file);
        assert_eq!(check_file(&file, None), Ok(()));
        std::fs::write(&file, b"gif").unwrap();
        assert_eq!(
            check_file(&file, None),
            Err(format!("{} already exists", file.display()))
        );
        let missing = dir.join("nodir");
        assert_eq!(
            check_file(&missing.join("demo.mp4"), None),
            Err(format!("{} does not exist", missing.display()))
        );
        assert!(check_file(&dir.join("demo.mp4"), Some(Format::Gif)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
