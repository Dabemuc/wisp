use crate::client::business_objects::{
    FocusDirection, ServerCommand, SessionCommand, SplitDirection,
};

const PREFIX: u8 = 0x02; // Ctrl-b

#[derive(Clone, Copy)]
enum InputState {
    Normal, // bytes pass through to the focused pane
    Prefix, // prefix seen; the NEXT byte is a command
}

pub struct CommandStateMachine {
    state: InputState,
}

impl CommandStateMachine {
    pub fn new() -> Self {
        Self {
            state: InputState::Normal,
        }
    }

    pub fn parse_input(&mut self, bytes: Vec<u8>) -> (Vec<ServerCommand>, Vec<u8>) {
        let mut pass: Vec<u8> = Vec::new();
        let mut commands: Vec<ServerCommand> = Vec::new();

        for b in bytes {
            // Copy the state OUT before matching, so the arms can call &mut self freely.
            match self.state {
                InputState::Normal => {
                    if b == PREFIX {
                        self.state = InputState::Prefix;
                    } else {
                        pass.push(b); // ordinary key -> forward
                    }
                }
                InputState::Prefix => {
                    match b {
                        PREFIX => pass.push(PREFIX), // prefix,prefix -> send a literal Ctrl-b
                        b'"' => commands.push(
                            SessionCommand::SplitFocusedWindow(SplitDirection::SplitHorizontal)
                                .into(),
                        ),
                        b'%' => commands.push(
                            SessionCommand::SplitFocusedWindow(SplitDirection::SplitVertical)
                                .into(),
                        ),
                        b'c' => commands.push(SessionCommand::CreateNewWindow.into()),
                        b'h' => {
                            commands.push(SessionCommand::FocusPane(FocusDirection::Left).into())
                        }
                        b'j' => {
                            commands.push(SessionCommand::FocusPane(FocusDirection::Down).into())
                        }
                        b'k' => commands.push(SessionCommand::FocusPane(FocusDirection::Up).into()),
                        b'l' => {
                            commands.push(SessionCommand::FocusPane(FocusDirection::Right).into())
                        }
                        b'0'..=b'9' => {
                            let window_index = (b - b'0') as usize;
                            commands.push(SessionCommand::SwitchToWindow(window_index).into());
                        }
                        _ => {} // unknown command -> swallow
                    }
                    self.state = InputState::Normal;
                }
            }
        }

        (commands, pass)
    }
}
