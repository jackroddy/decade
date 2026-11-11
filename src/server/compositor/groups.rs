use crate::server::workspace::{self, Group};

use super::State;

impl State {
    pub(super) fn toggle_fold(&mut self, id: u32) {
        if let Some(group) = self.groups.iter_mut().find(|group| group.id == id) {
            group.folded = !group.folded;
            self.schedule_save();
        }
    }

    /// Add a group named `name` below the others.
    pub(super) fn add_group(&mut self, name: &str) -> Result<(), String> {
        workspace::check_name(name, self.group_taken(None, name))?;
        self.groups.push(Group {
            id: self.next_group,
            name: name.to_string(),
            folded: false,
        });
        self.next_group += 1;
        Ok(())
    }

    pub(super) fn rename_group(&mut self, id: u32, name: &str) -> Result<(), String> {
        workspace::check_name(name, self.group_taken(Some(id), name))?;
        let group = (self.groups.iter_mut())
            .find(|group| group.id == id)
            .ok_or("the group has gone")?;
        group.name = name.to_string();
        Ok(())
    }

    /// Whether a group other than `id` is named `name`.
    fn group_taken(&self, id: Option<u32>, name: &str) -> bool {
        (self.groups.iter()).any(|group| Some(group.id) != id && group.name == name)
    }

    /// Remove the group, and return its workspaces to ungrouped.
    pub(super) fn delete_group(&mut self, id: u32) {
        self.groups.retain(|group| group.id != id);
        for workspace in &mut self.workspaces {
            workspace.group = workspace.group.filter(|&group| group != id);
        }
        self.schedule_save();
    }

    /// Put workspace `id` in `group`, or take it out of its group for `None`.
    pub(super) fn move_to_group(&mut self, id: u32, group: Option<u32>) {
        let known = group.is_none_or(|group| self.groups.iter().any(|known| known.id == group));
        if let Some(index) = self.workspace_index(id)
            && known
            && self.workspaces[index].group != group
        {
            self.workspaces[index].group = group;
            self.schedule_save();
        }
    }

    /// Move group `id` to index `to` among the groups.
    pub(super) fn move_group(&mut self, id: u32, to: usize) {
        let Some(from) = self.groups.iter().position(|group| group.id == id) else {
            return;
        };
        if from != to {
            let moved = self.groups.remove(from);
            self.groups.insert(to.min(self.groups.len()), moved);
            self.schedule_save();
        }
    }
}
