use crate::daemon::ui::ButtonRef;

#[derive(Debug)]
pub enum UiEvent {
    ButtonTap(ButtonRef),
}

pub enum UiCommand {
    Refresh,
    PushPage(Vec<Option<ButtonRef>>),
    PopPage,
}

impl std::fmt::Debug for UiCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UiCommand::Refresh => f.write_str("Refresh"),
            UiCommand::PushPage(_) => f.write_str("PushPage"),
            UiCommand::PopPage => f.write_str("PopPage"),
        }
    }
}
