use decade_ui::overlay::Entry;

use crate::chrome::frame::Group;
use crate::chrome::rename::Named;

/// What a right click on decade's chrome landed on, with what its entries
/// depend on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Place {
    /// A pane's bar or outline.
    Pane {
        id: usize,
        pinned: bool,
        zoomed: bool,
        scratch: bool,
        screenshots: bool,
    },

    /// A workspace's card or tile in the sidebar, with the id of its group.
    Workspace {
        id: u32,
        name: String,
        clearable: bool,
        group: Option<u32>,
        asleep: bool,

        /// Whether an agent in the workspace is working.
        working: bool,
    },

    /// A group's header in the open sidebar, with whether any of its
    /// workspaces is awake, asleep, or has a working agent.
    Group {
        id: u32,
        awake: bool,
        asleep: bool,
        working: bool,
    },

    /// A tab's pill on the top bar, by index.
    Tab(usize),

    /// The open sidebar outside the cards and headers.
    Sidebar,
}

/// What an entry does when it runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Pin(usize),
    Zoom(usize),
    Scratch(usize),

    /// Turn the pane's accept screenshots mode on or off.
    Screenshots(usize),

    /// Return the workspace's agents that are done or need attention to idle.
    Clear(u32),
    Rename(Named),

    /// Ask for a name and add a group by that name below the others.
    NewGroup,

    /// Remove the group, returning its workspaces to ungrouped.
    DeleteGroup(u32),

    /// Put the workspace in the group, or take it out of its group for
    /// `None`.
    MoveToGroup {
        workspace: u32,
        group: Option<u32>,
    },

    /// Open the palette's directory browser to pick the workspace's
    /// directory.
    ChangeDirectory(u32),
    CloseWorkspace(u32),

    /// Put the workspace to sleep.
    Sleep(u32),

    /// Wake the sleeping workspace.
    Wake(u32),

    /// Put every workspace in the group to sleep.
    SleepGroup(u32),

    /// Wake every sleeping workspace in the group.
    WakeGroup(u32),
}

fn entry(label: &str, command: Command) -> Entry<Command> {
    Entry {
        label: label.to_string(),
        action: command,
        destructive: matches!(command, Command::DeleteGroup(_)),
        confirm: None,
    }
}

/// The entries of the menu for `place`, in the order they are listed, with
/// `groups` the sidebar's groups in order.
pub fn entries(place: Place, groups: &[Group]) -> Vec<Entry<Command>> {
    match place {
        Place::Pane {
            id,
            pinned,
            zoomed,
            scratch,
            screenshots,
        } => vec![
            entry(if pinned { "Unpin" } else { "Pin" }, Command::Pin(id)),
            entry(if zoomed { "Unzoom" } else { "Zoom" }, Command::Zoom(id)),
            entry(
                if scratch {
                    "Unmark as scratch"
                } else {
                    "Mark as scratch"
                },
                Command::Scratch(id),
            ),
            entry(
                if screenshots {
                    "Stop accepting screenshots"
                } else {
                    "Accept screenshots"
                },
                Command::Screenshots(id),
            ),
            entry("Rename", Command::Rename(Named::Pane(id))),
        ],
        Place::Workspace {
            id,
            name,
            clearable,
            group,
            asleep,
            working,
        } => {
            let mut entries = Vec::new();
            if clearable {
                entries.push(entry("Clear", Command::Clear(id)));
            }
            entries.push(entry("Rename", Command::Rename(Named::Workspace(id))));
            for other in groups.iter().filter(|other| Some(other.id) != group) {
                let command = Command::MoveToGroup {
                    workspace: id,
                    group: Some(other.id),
                };
                entries.push(entry(&format!("Move to {}", other.name), command));
            }
            if let Some(own) = groups.iter().find(|own| Some(own.id) == group) {
                let command = Command::MoveToGroup {
                    workspace: id,
                    group: None,
                };
                entries.push(entry(&format!("Move out of {}", own.name), command));
            }
            if asleep {
                entries.push(entry("Wake", Command::Wake(id)));
            } else {
                entries.push(Entry {
                    confirm: working.then(|| format!("Sleep {name} while its agent works")),
                    ..entry("Sleep", Command::Sleep(id))
                });
            }
            entries.push(entry("Change directory…", Command::ChangeDirectory(id)));
            entries.push(Entry {
                confirm: Some(format!("Close {name} and its panes")),
                ..entry("Close…", Command::CloseWorkspace(id))
            });
            entries
        }
        Place::Group {
            id,
            awake,
            asleep,
            working,
        } => {
            let mut entries = vec![entry("Rename", Command::Rename(Named::Group(id)))];
            if awake {
                let name = (groups.iter())
                    .find(|group| group.id == id)
                    .map_or("the group", |group| group.name.as_str());
                entries.push(Entry {
                    confirm: working.then(|| format!("Sleep {name} while an agent works")),
                    ..entry("Sleep all", Command::SleepGroup(id))
                });
            }
            if asleep {
                entries.push(entry("Wake all", Command::WakeGroup(id)));
            }
            entries.push(entry("Delete", Command::DeleteGroup(id)));
            entries
        }
        Place::Tab(index) => vec![entry("Rename", Command::Rename(Named::Tab(index)))],
        Place::Sidebar => vec![entry("New group", Command::NewGroup)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(place: &Place) -> Vec<String> {
        (entries(place.clone(), &groups()).into_iter())
            .map(|entry| entry.label)
            .collect()
    }

    fn groups() -> Vec<Group> {
        let group = |id, name: &str| Group {
            id,
            name: name.to_string(),
            folded: false,
        };
        vec![group(7, "work"), group(9, "home")]
    }

    #[test]
    fn a_pane_offers_pin_zoom_scratch_screenshots_and_rename_labelled_for_what_a_click_does() {
        let plain = Place::Pane {
            id: 4,
            pinned: false,
            zoomed: false,
            scratch: false,
            screenshots: false,
        };
        assert_eq!(
            labels(&plain),
            [
                "Pin",
                "Zoom",
                "Mark as scratch",
                "Accept screenshots",
                "Rename"
            ]
        );
        let commands: Vec<Command> = entries(plain, &[]).iter().map(|e| e.action).collect();
        assert_eq!(
            commands,
            [
                Command::Pin(4),
                Command::Zoom(4),
                Command::Scratch(4),
                Command::Screenshots(4),
                Command::Rename(Named::Pane(4))
            ]
        );
        let set = Place::Pane {
            id: 4,
            pinned: true,
            zoomed: true,
            scratch: true,
            screenshots: true,
        };
        assert_eq!(
            labels(&set),
            [
                "Unpin",
                "Unzoom",
                "Unmark as scratch",
                "Stop accepting screenshots",
                "Rename"
            ]
        );
    }

    fn workspace(clearable: bool, group: Option<u32>) -> Place {
        Place::Workspace {
            id: 2,
            name: "decade".to_string(),
            clearable,
            group,
            asleep: false,
            working: false,
        }
    }

    fn group(awake: bool, asleep: bool, working: bool) -> Place {
        Place::Group {
            id: 7,
            awake,
            asleep,
            working,
        }
    }

    #[test]
    fn a_workspace_offers_rename_and_clear_only_when_its_agent_is_done_or_needs_attention() {
        let rename = entry("Rename", Command::Rename(Named::Workspace(2)));
        let clear = entry("Clear", Command::Clear(2));
        assert_eq!(
            entries(workspace(true, None), &[])[..2],
            [clear, rename.clone()]
        );
        assert_eq!(entries(workspace(false, None), &[])[..1], [rename]);
    }

    #[test]
    fn a_workspace_ends_with_change_directory_then_a_close_that_asks_for_a_second_click() {
        let entries = entries(workspace(true, Some(7)), &groups());
        let [.., change, close] = entries.as_slice() else {
            panic!("fewer than two entries");
        };
        assert_eq!(
            *change,
            entry("Change directory…", Command::ChangeDirectory(2))
        );
        assert_eq!(close.label, "Close…");
        assert_eq!(close.action, Command::CloseWorkspace(2));
        assert_eq!(close.confirm.as_deref(), Some("Close decade and its panes"));
    }

    #[test]
    fn a_workspace_offers_a_move_to_every_other_group_and_out_of_its_own() {
        assert_eq!(
            labels(&workspace(false, None)),
            [
                "Rename",
                "Move to work",
                "Move to home",
                "Sleep",
                "Change directory…",
                "Close…"
            ]
        );
        let at_work = workspace(false, Some(7));
        assert_eq!(
            labels(&at_work),
            [
                "Rename",
                "Move to home",
                "Move out of work",
                "Sleep",
                "Change directory…",
                "Close…"
            ]
        );
        let commands: Vec<Command> = (entries(at_work, &groups()).into_iter())
            .map(|entry| entry.action)
            .collect();
        assert_eq!(
            commands[1..3],
            [
                Command::MoveToGroup {
                    workspace: 2,
                    group: Some(9)
                },
                Command::MoveToGroup {
                    workspace: 2,
                    group: None
                }
            ]
        );
    }

    #[test]
    fn an_awake_workspace_offers_sleep_asking_twice_while_its_agent_works_and_an_asleep_one_wake() {
        let place = |asleep, working| Place::Workspace {
            id: 2,
            name: "decade".to_string(),
            clearable: false,
            group: None,
            asleep,
            working,
        };
        let sleep = |working| {
            let entries = entries(place(false, working), &[]);
            entries.into_iter().find(|entry| entry.label == "Sleep")
        };
        assert_eq!(sleep(false), Some(entry("Sleep", Command::Sleep(2))));
        let asking = sleep(true).unwrap();
        assert_eq!(asking.action, Command::Sleep(2));
        assert_eq!(
            asking.confirm.as_deref(),
            Some("Sleep decade while its agent works")
        );
        let entries = entries(place(true, false), &[]);
        assert!(entries.contains(&entry("Wake", Command::Wake(2))));
        assert!(!entries.iter().any(|entry| entry.label == "Sleep"));
    }

    #[test]
    fn a_group_offers_sleep_all_while_a_workspace_is_awake_and_wake_all_while_one_is_asleep() {
        let labels = |place| -> Vec<String> {
            (entries(place, &groups()).into_iter())
                .map(|entry| entry.label)
                .collect()
        };
        assert_eq!(
            labels(group(true, true, false)),
            ["Rename", "Sleep all", "Wake all", "Delete"]
        );
        assert_eq!(
            labels(group(true, false, false)),
            ["Rename", "Sleep all", "Delete"]
        );
        assert_eq!(
            labels(group(false, true, false)),
            ["Rename", "Wake all", "Delete"]
        );
        assert_eq!(labels(group(false, false, false)), ["Rename", "Delete"]);
        let quiet = entries(group(true, true, false), &groups());
        assert_eq!(quiet[1], entry("Sleep all", Command::SleepGroup(7)));
        assert_eq!(quiet[2], entry("Wake all", Command::WakeGroup(7)));
        let working = entries(group(true, false, true), &groups());
        assert_eq!(
            working[1].confirm.as_deref(),
            Some("Sleep work while an agent works")
        );
    }

    #[test]
    fn a_group_header_offers_rename_and_delete_and_empty_sidebar_space_a_new_group() {
        assert_eq!(
            entries(group(false, false, false), &groups()),
            [
                entry("Rename", Command::Rename(Named::Group(7))),
                entry("Delete", Command::DeleteGroup(7))
            ]
        );
        assert_eq!(
            entries(Place::Sidebar, &groups()),
            [entry("New group", Command::NewGroup)]
        );
    }

    #[test]
    fn delete_is_the_only_destructive_entry() {
        let destructive: Vec<String> = (entries(group(true, true, true), &groups()).into_iter())
            .filter(|entry| entry.destructive)
            .map(|entry| entry.label)
            .collect();
        assert_eq!(destructive, ["Delete"]);
        let pane = Place::Pane {
            id: 4,
            pinned: false,
            zoomed: false,
            scratch: false,
            screenshots: false,
        };
        assert!(!entries(pane, &[]).iter().any(|entry| entry.destructive));
    }

    #[test]
    fn a_tab_pill_offers_rename() {
        assert_eq!(
            entries(Place::Tab(1), &[]),
            [entry("Rename", Command::Rename(Named::Tab(1)))]
        );
    }
}
