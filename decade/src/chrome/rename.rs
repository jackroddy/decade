/// Something the person can rename.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Named {
    Workspace(u32),

    /// A tab of the workspace on screen, by index.
    Tab(usize),

    /// A pane of the tab on screen, by id.
    Pane(usize),

    /// A group in the sidebar, by id.
    Group(u32),

    /// The group the field adds once a name is typed in it.
    NewGroup,
}

impl Named {
    pub fn title(self) -> &'static str {
        match self {
            Named::Workspace(_) => "Rename workspace",
            Named::Tab(_) => "Rename tab",
            Named::Pane(_) => "Rename pane",
            Named::Group(_) => "Rename group",
            Named::NewGroup => "New group",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_names_what_is_renamed() {
        assert_eq!(Named::Pane(3).title(), "Rename pane");
        assert_eq!(Named::NewGroup.title(), "New group");
    }
}
