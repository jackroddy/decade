use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::files;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct App {
    pub name: String,
    pub argv: Vec<String>,
}

pub fn installed() -> Vec<App> {
    let mut found: HashMap<String, App> = HashMap::new();
    for dir in application_dirs() {
        let mut files = Vec::new();
        desktop_files(&dir, &mut files);
        for path in files {
            let Some(id) = path.strip_prefix(&dir).ok().and_then(|id| id.to_str()) else {
                continue;
            };
            let id = id.replace('/', "-");
            if found.contains_key(&id) {
                continue;
            }
            if let Some(app) = fs::read_to_string(&path).ok().and_then(|text| parse(&text)) {
                found.insert(id, app);
            }
        }
    }
    let mut apps: Vec<App> = found.into_values().collect();
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

fn application_dirs() -> Vec<PathBuf> {
    let home = files::xdg_home("XDG_DATA_HOME", ".local/share");
    let system = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    home.into_iter()
        .chain(system.split(':').map(PathBuf::from))
        .map(|dir| dir.join("applications"))
        .collect()
}

fn desktop_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            desktop_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "desktop") {
            out.push(path);
        }
    }
}

fn parse(text: &str) -> Option<App> {
    let mut in_entry = false;
    let mut keys: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            keys.entry(key.trim()).or_insert(value.trim());
        }
    }
    let flag = |key: &str| keys.get(key).is_some_and(|value| *value == "true");
    if keys.get("Type") != Some(&"Application")
        || flag("NoDisplay")
        || flag("Hidden")
        || flag("Terminal")
    {
        return None;
    }
    let argv = split_exec(keys.get("Exec")?);
    if argv.is_empty() {
        return None;
    }
    Some(App {
        name: keys.get("Name")?.to_string(),
        argv,
    })
}

fn split_exec(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = exec.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => word.extend(chars.next()),
            ' ' if !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '%' => {
                if chars.next() == Some('%') {
                    word.push('%');
                    started = true;
                }
            }
            _ => {
                word.push(ch);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words.retain(|word| !word.is_empty());
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_entry_gives_its_name_and_command() {
        let text = "[Desktop Entry]\nType=Application\nName=Files\nName[de]=Dateien\nExec=nautilus --new-window %U\n\n[Desktop Action new]\nName=Other\nExec=other\n";
        let app = parse(text).unwrap();
        assert_eq!(app.name, "Files");
        assert_eq!(app.argv, ["nautilus", "--new-window"]);
    }

    #[test]
    fn hidden_terminal_and_non_app_entries_are_skipped() {
        let entry = |extra: &str| format!("[Desktop Entry]\nName=x\nExec=x\n{extra}\n");
        assert!(parse(&entry("Type=Application")).is_some());
        assert!(parse(&entry("Type=Application\nNoDisplay=true")).is_none());
        assert!(parse(&entry("Type=Application\nHidden=true")).is_none());
        assert!(parse(&entry("Type=Application\nTerminal=true")).is_none());
        assert!(parse(&entry("Type=Link")).is_none());
    }

    #[test]
    fn exec_splits_on_spaces_outside_quotes_and_drops_field_codes() {
        assert_eq!(
            split_exec(r#"env "A B=1" app --name="x y" %f 100%%"#),
            ["env", "A B=1", "app", "--name=x y", "100%"]
        );
        assert_eq!(split_exec("app %F"), ["app"]);
    }
}
