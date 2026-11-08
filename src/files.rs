use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
struct Version {
    version: Option<u32>,
}

/// The name of decade's directory under each XDG base directory.
pub const DIR: &str = "decade";

/// The directory for sockets: `$XDG_RUNTIME_DIR`, or the temp directory.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// The directory in `$var`, or `fallback` under `$HOME`, or `None` when neither is set.
pub fn xdg_home(var: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
}

/// The file's text, or `None` when there is no file.
pub fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// The top-level `version` of a TOML file.
pub fn version(text: &str) -> Result<u32, String> {
    let version: Version = toml::from_str(text).map_err(|error| error.to_string())?;
    version
        .version
        .ok_or_else(|| "the file has no version".to_string())
}

/// Replace the file at `path` with `text` by a rename, creating its directory first.
pub fn write(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut partial = OsString::from(path);
    partial.push(".partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_read_from_the_top_of_the_file_and_its_absence_is_an_error() {
        assert_eq!(version("version = 2\nname = \"x\"\n"), Ok(2));
        assert!(version("name = \"x\"\n").is_err());
        assert!(version("version = \"two\"\n").is_err());
    }

    #[test]
    fn a_written_file_reads_back_and_a_missing_one_reads_as_none() {
        let dir = std::env::temp_dir().join(format!("decade-files-{}", std::process::id()));
        let path = dir.join("nested").join("file.toml");
        assert_eq!(read(&path), Ok(None));
        write(&path, "version = 1\n").unwrap();
        write(&path, "version = 2\n").unwrap();
        assert_eq!(read(&path), Ok(Some("version = 2\n".to_string())));
        assert!(!dir.join("nested").join("file.toml.partial").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
