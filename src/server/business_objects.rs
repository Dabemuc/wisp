use crate::common::dtos::{FocusDirectionDTO, ServerCommandDTO, SplitDirectionDTO};

#[derive(Clone, Copy)]
pub enum ServerCommand {
    // No commands to handle at server level yet. Only pass through to session
    Session(SessionCommand),
}

impl From<ServerCommandDTO> for ServerCommand {
    fn from(dto: ServerCommandDTO) -> Self {
        match dto {
            ServerCommandDTO::SplitFocusedWindow(d) => {
                Self::Session(SessionCommand::SplitFocusedWindow(d.into()))
            }
            ServerCommandDTO::CreateNewWindow => Self::Session(SessionCommand::CreateNewWindow),
            ServerCommandDTO::SwitchToWindow(id) => {
                Self::Session(SessionCommand::SwitchToWindow(id))
            }
            ServerCommandDTO::FocusPane(d) => Self::Session(SessionCommand::FocusPane(d.into())),
        }
    }
}

#[derive(Clone, Copy)]
pub enum SessionCommand {
    SplitFocusedWindow(SplitDirection),
    CreateNewWindow,
    SwitchToWindow(usize),
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
