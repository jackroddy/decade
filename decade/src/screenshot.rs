use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use decade_channel::{Channel, Message, Screenshot};

/// Ask decade for a PNG of pane `pane` and write it to the new file
/// `file`.
pub fn run(pane: &str, file: &str) -> Result<(), String> {
    check_file(Path::new(file))?;
    let socket = std::env::var_os("DECADE_SOCKET")
        .ok_or("DECADE_SOCKET is not set; run decade screenshot in a program decade started")?;
    let (mut channel, messages) = Channel::connect(
        Path::new(&socket),
        "decade screenshot",
        &[],
        &[],
        None,
        None,
    )
    .map_err(|error| format!("connecting to decade failed: {error}"))?;
    let screenshot = Screenshot {
        pane: pane.to_string(),
        file: std::path::absolute(file)
            .map_err(|error| format!("{file}: {error}"))?
            .display()
            .to_string(),
        caller: std::env::var("DECADE_PANE").ok(),
    };
    channel
        .screenshot(&screenshot)
        .map_err(|error| format!("asking decade for a screenshot failed: {error}"))?;
    let png = loop {
        match messages.recv() {
            Ok(Message::Screenshot { png }) => break png,
            Ok(Message::Error { reason, .. }) => return Err(reason),
            Ok(Message::Closed) | Err(_) => {
                return Err("decade closed the connection without a screenshot".to_string());
            }
            Ok(_) => {}
        }
    };
    write_new(Path::new(file), &png).map_err(|error| format!("{file}: {error}"))
}

/// Refuse `file` unless it ends in `.png`, its directory exists and
/// nothing is at its path.
fn check_file(file: &Path) -> Result<(), String> {
    let shown = file.display();
    if file.extension().is_none_or(|extension| extension != "png") {
        return Err(format!("{shown} does not end in .png"));
    }
    if let Some(dir) = file.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        match std::fs::metadata(dir) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(format!("{} is not a directory", dir.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(format!("{} does not exist", dir.display()));
            }
            Err(error) => return Err(format!("{}: {error}", dir.display())),
        }
    }
    match std::fs::symlink_metadata(file) {
        Ok(_) => Err(format!("{shown} already exists")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{shown}: {error}")),
    }
}

fn write_new(file: &Path, png: &[u8]) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)?
        .write_all(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_does_not_end_in_png_is_refused() {
        for file in ["shot.jpg", "shot", "shot.png.txt", ".png"] {
            let refused = check_file(Path::new(file)).unwrap_err();
            assert_eq!(refused, format!("{file} does not end in .png"));
        }
    }

    #[test]
    fn a_file_or_symlink_already_at_the_path_is_refused_and_a_free_path_is_not() {
        let dir =
            std::env::temp_dir().join(format!("decade-screenshot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shot.png");
        let link = dir.join("link.png");
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&link);
        assert_eq!(check_file(&file), Ok(()));
        std::os::unix::fs::symlink(&file, &link).unwrap();

        // the link dangles, as its target is not written yet
        let refused = check_file(&link).unwrap_err();
        assert!(refused.ends_with("link.png already exists"));
        write_new(&file, b"png").unwrap();
        assert!(check_file(&file).unwrap_err().ends_with("already exists"));
        assert_eq!(
            write_new(&file, b"png").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_in_a_directory_that_is_missing_or_not_a_directory_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("decade-screenshot-dir-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("nodir");
        let _ = std::fs::remove_dir(&missing);
        let refused = check_file(&missing.join("shot.png")).unwrap_err();
        assert_eq!(refused, format!("{} does not exist", missing.display()));
        let plain = dir.join("plain");
        std::fs::write(&plain, b"").unwrap();
        let refused = check_file(&plain.join("shot.png")).unwrap_err();
        assert_eq!(refused, format!("{} is not a directory", plain.display()));
        assert_eq!(check_file(Path::new("shot-that-is-not-here.png")), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
