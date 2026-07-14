use crate::window_handle::{FocusDirection, SplitDirection};

const PREFIX: u8 = 0x02; // Ctrl-b

#[derive(Clone, Copy)]
enum InputState {
    Normal, // bytes pass through to the focused pane
    Prefix, // prefix seen; the NEXT byte is a command
}

pub enum WispCommand {
    SplitFocusedWindow(SplitDirection),
    CreateNewWindow,
    SwitchToWindow(usize),
    FocusPane(FocusDirection),
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

    pub fn parse_input(&mut self, bytes: &[u8]) -> (Vec<WispCommand>, Vec<u8>) {
        let mut pass: Vec<u8> = Vec::new();
        let mut commands: Vec<WispCommand> = Vec::new();

        for &b in bytes {
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
                        b'd' => commands.push(WispCommand::SplitFocusedWindow(
                            SplitDirection::SplitHorizontal,
                        )),
                        b'r' => commands.push(WispCommand::SplitFocusedWindow(
                            SplitDirection::SplitVertical,
                        )),
                        b'c' => commands.push(WispCommand::CreateNewWindow),
                        b'h' => commands.push(WispCommand::FocusPane(FocusDirection::Left)),
                        b'j' => commands.push(WispCommand::FocusPane(FocusDirection::Down)),
                        b'k' => commands.push(WispCommand::FocusPane(FocusDirection::Up)),
                        b'l' => commands.push(WispCommand::FocusPane(FocusDirection::Right)),
                        b'0'..=b'9' => {
                            let window_index = (b - b'0') as usize;
                            commands.push(WispCommand::SwitchToWindow(window_index));
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
