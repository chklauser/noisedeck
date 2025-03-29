use crate::daemon::ui::ButtonRef;

#[derive(Debug)]
pub enum UiEvent {
    ButtonTap(ButtonRef),
    ButtonHold(ButtonRef),
}

pub enum UiCommand {
    Refresh,
    Flip(Vec<Option<ButtonRef>>),
}

impl std::fmt::Debug for UiCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UiCommand::Refresh => f.write_str("Refresh"),
            UiCommand::Flip(_) => f.write_str("PushPage"),
        }
    }
}
