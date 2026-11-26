use decade_ui::overlay::{Choice, Dialog};

/// How the person answers decade's question whether an agent may capture
/// a pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Once,

    /// Allow this one and turn on the pane's accept captures mode.
    Always,
    Deny,
}

const fn choice(answer: Answer, label: &'static str, key: char) -> Choice<Answer> {
    Choice {
        answer,
        label,
        key,
        destructive: matches!(answer, Answer::Deny),
    }
}

static CHOICES: [Choice<Answer>; 3] = [
    choice(Answer::Once, "Allow once", 'y'),
    choice(Answer::Always, "Allow and accept captures", 'a'),
    choice(Answer::Deny, "Deny", 'n'),
];

/// The answer a key gives, for y, a and n.
pub fn answer_for(key: char) -> Option<Answer> {
    (CHOICES.iter())
        .find(|choice| choice.key == key)
        .map(|choice| choice.answer)
}

/// The dialog that asks the person whether `agent` may take a screenshot
/// of pane `pane` in workspace `workspace`. The caller sets the rest.
pub fn dialog(agent: &str, workspace: &str, pane: &str) -> Dialog<'static, Answer> {
    Dialog {
        question: format!("{agent} asks for a screenshot"),
        detail: format!("of pane {pane} in workspace {workspace}"),
        path: None,
        choices: &CHOICES,
        escape: Answer::Deny,
        armed: false,
        selected: 0,
        window: (0, 0),
        cell: (0, 0),
    }
}

/// The dialog that asks the person whether `agent` may save a screenshot
/// of pane `pane` to `file`. The caller sets the rest.
pub fn save_dialog(agent: &str, pane: &str, file: &str) -> Dialog<'static, Answer> {
    Dialog {
        question: format!("{agent} asks to save a screenshot"),
        detail: format!("of pane {pane} to"),
        path: Some(file.to_string()),
        choices: &CHOICES,
        escape: Answer::Deny,
        armed: false,
        selected: 0,
        window: (0, 0),
        cell: (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn y_a_and_n_answer_and_other_keys_do_not() {
        assert_eq!(answer_for('y'), Some(Answer::Once));
        assert_eq!(answer_for('a'), Some(Answer::Always));
        assert_eq!(answer_for('n'), Some(Answer::Deny));
        assert_eq!(answer_for('x'), None);
    }

    #[test]
    fn escape_denies_and_only_deny_is_red() {
        let dialog = dialog("coax", "decade", "terminal");
        assert_eq!(dialog.escape, Answer::Deny);
        let red: Vec<Answer> = (dialog.choices.iter())
            .filter(|choice| choice.destructive)
            .map(|choice| choice.answer)
            .collect();
        assert_eq!(red, [Answer::Deny]);
    }

    #[test]
    fn the_save_dialog_names_the_agent_the_pane_and_the_file() {
        let dialog = save_dialog("coax", "code", "/work/shot.png");
        let path = dialog.path.as_deref().unwrap_or_default();
        let text = format!("{} {} {path}", dialog.question, dialog.detail);
        assert_eq!(
            text,
            "coax asks to save a screenshot of pane code to /work/shot.png"
        );
        let labels: Vec<&str> = dialog.choices.iter().map(|choice| choice.label).collect();
        assert_eq!(labels, ["Allow once", "Allow and accept captures", "Deny"]);
        assert_eq!(dialog.escape, Answer::Deny);
    }
}
