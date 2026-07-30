use crate::common::dtos::{FocusDirectionDTO, ServerCommandDTO, SplitDirectionDTO, TermSizeDTO};

#[derive(Clone, Copy)]
pub enum ServerCommand {
    KillServer,
    ListSessions,
    Attach(TermSize),
    Session(SessionCommand),
}

impl From<ServerCommand> for ServerCommandDTO {
    fn from(c: ServerCommand) -> Self {
        match c {
            ServerCommand::KillServer => Self::KillServer,
            ServerCommand::ListSessions => Self::ListSessions,
            ServerCommand::Attach(s) => Self::Attach(s.into()),
            ServerCommand::Session(sc) => sc.into(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct TermSize {
    pub rows: u16,
    pub cols: u16,
}

impl From<TermSize> for TermSizeDTO {
    fn from(ts: TermSize) -> Self {
        Self {
            rows: ts.rows,
            cols: ts.cols,
        }
    }
}

#[derive(Clone, Copy)]
pub enum SessionCommand {
    SplitFocusedWindow(SplitDirection),
    CreateNewWindow,
    SwitchToWindow(u16),
    FocusPane(FocusDirection),
}

impl From<SessionCommand> for ServerCommandDTO {
    fn from(c: SessionCommand) -> Self {
        match c {
            SessionCommand::SplitFocusedWindow(d) => Self::SplitFocusedWindow(d.into()),
            SessionCommand::CreateNewWindow => Self::CreateNewWindow,
            SessionCommand::SwitchToWindow(id) => Self::SwitchToWindow(id),
            SessionCommand::FocusPane(d) => Self::FocusPane(d.into()),
        }
    }
}

impl From<SessionCommand> for ServerCommand {
    fn from(c: SessionCommand) -> Self {
        ServerCommand::Session(c)
    }
}

#[derive(Clone, Copy)]
pub enum SplitDirection {
    SplitHorizontal,
    SplitVertical,
}

impl From<SplitDirection> for SplitDirectionDTO {
    fn from(d: SplitDirection) -> Self {
        match d {
            SplitDirection::SplitHorizontal => Self::SplitHorizontal,
            SplitDirection::SplitVertical => Self::SplitVertical,
        }
    }
}

#[derive(Clone, Copy)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

impl From<FocusDirection> for FocusDirectionDTO {
    fn from(d: FocusDirection) -> Self {
        match d {
            FocusDirection::Left => Self::Left,
            FocusDirection::Right => Self::Right,
            FocusDirection::Up => Self::Up,
            FocusDirection::Down => Self::Down,
        }
    }
}
