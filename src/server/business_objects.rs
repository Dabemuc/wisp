use crate::common::dtos::{FocusDirectionDTO, ServerCommandDTO, SplitDirectionDTO, TermSizeDTO};

#[derive(Clone, Copy)]
pub enum ServerCommand {
    KillServer,
    ListSessions,
    Attach(TermSize),
    Session(SessionCommand),
}

impl From<ServerCommandDTO> for ServerCommand {
    fn from(dto: ServerCommandDTO) -> Self {
        match dto {
            ServerCommandDTO::KillServer => Self::KillServer,
            ServerCommandDTO::ListSessions => Self::ListSessions,
            ServerCommandDTO::Attach(ts) => Self::Attach(ts.into()),
            ServerCommandDTO::SplitFocusedWindow(d) => {
                Self::Session(SessionCommand::SplitFocusedWindow(d.into()))
            }
            ServerCommandDTO::CreateNewWindow => Self::Session(SessionCommand::CreateNewWindow),
            ServerCommandDTO::SwitchToWindow(id) => {
                Self::Session(SessionCommand::SwitchToWindow(id.into()))
            }
            ServerCommandDTO::FocusPane(d) => Self::Session(SessionCommand::FocusPane(d.into())),
        }
    }
}

#[derive(Clone, Copy)]
pub struct TermSize {
    pub rows: u16,
    pub cols: u16,
}

impl From<TermSizeDTO> for TermSize {
    fn from(ts: TermSizeDTO) -> Self {
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
    SwitchToWindow(usize), // TODO: Probably switch to u16
    FocusPane(FocusDirection),
}

#[derive(Clone, Copy)]
pub enum SplitDirection {
    SplitHorizontal,
    SplitVertical,
}

impl From<SplitDirectionDTO> for SplitDirection {
    fn from(dto: SplitDirectionDTO) -> Self {
        match dto {
            SplitDirectionDTO::SplitHorizontal => Self::SplitHorizontal,
            SplitDirectionDTO::SplitVertical => Self::SplitVertical,
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

impl From<FocusDirectionDTO> for FocusDirection {
    fn from(dto: FocusDirectionDTO) -> Self {
        match dto {
            FocusDirectionDTO::Left => Self::Left,
            FocusDirectionDTO::Right => Self::Right,
            FocusDirectionDTO::Up => Self::Up,
            FocusDirectionDTO::Down => Self::Down,
        }
    }
}
