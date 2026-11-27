use decade_ui::overlay::{Choice, Dialog};

/// How the person answers decade's question whether an agent may capture
/// a pane or play input in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Once,

    /// Allow this one and turn on the pane's mode the dialog names.
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

static INPUT_CHOICES: [Choice<Answer>; 3] = [
    choice(Answer::Once, "Allow once", 'y'),
    choice(Answer::Always, "Allow and accept input", 'a'),
    choice(Answer::Deny, "Deny", 'n'),
];

static RESIZE_CHOICES: [Choice<Answer>; 2] = [
    choice(Answer::Once, "Allow", 'y'),
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

/// The dialog that asks the person whether `agent` may record pane `pane`
/// into `file` for `duration`, or until stopped, up to `limit`, when
/// `duration` is `None`. The caller sets the rest.
pub fn record_dialog(
    agent: &str,
    pane: &str,
    duration: Option<&str>,
    limit: &str,
    file: &str,
) -> Dialog<'static, Answer> {
    let detail = match duration {
        Some(duration) => format!("for {duration} to"),
        None => format!("until stopped, up to {limit}, to"),
    };
    Dialog {
        question: format!("{agent} asks to record pane {pane}"),
        detail,
        path: Some(file.to_string()),
        choices: &CHOICES,
        escape: Answer::Deny,
        armed: false,
        selected: 0,
        window: (0, 0),
        cell: (0, 0),
    }
}

/// The dialog that asks the person whether `agent` may play a script of
/// `steps` steps lasting `span`, or taking no time for `None`, in pane
/// `pane`. The caller sets the rest.
pub fn input_dialog(
    agent: &str,
    pane: &str,
    steps: usize,
    span: Option<&str>,
) -> Dialog<'static, Answer> {
    let steps = match steps {
        1 => "1 step".to_string(),
        steps => format!("{steps} steps"),
    };

    // the warning leads, as the dialog cuts the line's end
    // when the window is narrow
    let detail = match span {
        Some(span) => format!("Escape stops it · {steps} over {span}"),
        None => format!("Escape stops it · {steps} at once"),
    };
    Dialog {
        question: format!("{agent} asks to control pane {pane}"),
        detail,
        path: None,
        choices: &INPUT_CHOICES,
        escape: Answer::Deny,
        armed: false,
        selected: 0,
        window: (0, 0),
        cell: (0, 0),
    }
}

/// The dialog that asks the person whether `agent` may resize pane
/// `pane` from `from` to `to` pixels. It offers no mode, so decade asks
/// every time. The caller sets the rest.
pub fn resize_dialog(
    agent: &str,
    pane: &str,
    from: (i32, i32),
    to: (u32, u32),
) -> Dialog<'static, Answer> {
    Dialog {
        question: format!("{agent} asks to resize pane {pane}"),
        detail: format!("from {}x{} to {}x{} pixels", from.0, from.1, to.0, to.1),
        path: None,
        choices: &RESIZE_CHOICES,
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

    #[test]
    fn the_input_dialog_names_the_agent_the_pane_the_steps_and_its_own_mode() {
        let dialog = input_dialog("coax", "scratch", 12, Some("4.5 seconds"));
        assert_eq!(dialog.question, "coax asks to control pane scratch");
        assert_eq!(dialog.detail, "Escape stops it · 12 steps over 4.5 seconds");
        let labels: Vec<&str> = dialog.choices.iter().map(|choice| choice.label).collect();
        assert_eq!(labels, ["Allow once", "Allow and accept input", "Deny"]);
        let keys: Vec<char> = dialog.choices.iter().map(|choice| choice.key).collect();
        assert_eq!(keys, ['y', 'a', 'n']);
        assert_eq!(dialog.escape, Answer::Deny);
        let one = input_dialog("An agent", "scratch", 1, None);
        assert_eq!(one.detail, "Escape stops it · 1 step at once");
    }

    #[test]
    fn the_resize_dialog_names_both_sizes_and_offers_only_allow_and_deny() {
        let dialog = resize_dialog("coax", "scratch", (812, 590), (1200, 800));
        assert_eq!(dialog.question, "coax asks to resize pane scratch");
        assert_eq!(dialog.detail, "from 812x590 to 1200x800 pixels");
        let keys: Vec<char> = dialog.choices.iter().map(|choice| choice.key).collect();
        assert_eq!(keys, ['y', 'n']);
        assert_eq!(dialog.escape, Answer::Deny);
    }

    #[test]
    fn the_record_dialog_names_the_pane_the_time_and_the_file() {
        let text = |duration| {
            let dialog = record_dialog("coax", "code", duration, "1 minute", "/work/demo.gif");
            let path = dialog.path.as_deref().unwrap_or_default();
            format!("{} {} {path}", dialog.question, dialog.detail)
        };
        assert_eq!(
            text(Some("4.5 seconds")),
            "coax asks to record pane code for 4.5 seconds to /work/demo.gif"
        );
        assert_eq!(
            text(None),
            "coax asks to record pane code until stopped, up to 1 minute, to /work/demo.gif"
        );
    }
}
