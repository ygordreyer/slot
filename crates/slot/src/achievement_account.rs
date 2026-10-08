use slot_achievements::{AccountControl, AccountState};
use slot_input::Btn;
use slot_ui::{AccountMenu, Keyboard, KeyboardInput, KeyboardResult, UndoFace};
use zeroize::{Zeroize, Zeroizing};

pub enum AccountEffect {
    Back,
    Control(AccountControl),
}

pub struct AccountScreen {
    pub account: AccountState,
    pub selected: usize,
    pub username: String,
    pub keyboard: Option<Keyboard>,
    pub status: String,
    password: Zeroizing<String>,
    revision: u64,
}

impl AccountScreen {
    pub fn new(account: AccountState) -> Self {
        Self {
            username: account.username.clone(),
            status: account.message.clone(),
            account,
            selected: 0,
            keyboard: None,
            revision: 0,
            password: Zeroizing::new(String::new()),
        }
    }

    pub fn observe(&mut self, account: AccountState) {
        if account != self.account {
            self.revision = self.revision.wrapping_add(1);
            if account.signed_in && !self.account.signed_in {
                self.close();
            }
            if account.signed_in || (self.account.signed_in && !account.signed_in) {
                self.username = account.username.clone();
            }
            self.status = account.message.clone();
            self.account = account;
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn face(&self) -> UndoFace {
        if let Some(keyboard) = &self.keyboard {
            return keyboard.face();
        }
        AccountMenu {
            enabled: self.account.enabled,
            username: &self.username,
            password_length: self.password.len(),
            selected: self.selected,
            status: &self.status,
        }
        .face()
    }

    pub fn close(&mut self) {
        self.password.zeroize();
        self.keyboard = None;
    }

    pub fn input(&mut self, button: Btn) -> Option<AccountEffect> {
        self.revision = self.revision.wrapping_add(1);
        if let Some(keyboard) = &mut self.keyboard {
            let input = match button {
                Btn::Up => KeyboardInput::Up,
                Btn::Down => KeyboardInput::Down,
                Btn::Left => KeyboardInput::Left,
                Btn::Right => KeyboardInput::Right,
                Btn::A => KeyboardInput::Type,
                Btn::B => KeyboardInput::Delete,
                Btn::X => KeyboardInput::Caps,
                Btn::Y => KeyboardInput::Symbols,
                Btn::L1 => KeyboardInput::CursorLeft,
                Btn::R1 => KeyboardInput::CursorRight,
                Btn::Start => KeyboardInput::Confirm,
                Btn::Select => KeyboardInput::Reveal,
                _ => return None,
            };
            match keyboard.input(input) {
                KeyboardResult::Editing => {}
                KeyboardResult::Cancelled => self.keyboard = None,
                KeyboardResult::Confirmed(text) => {
                    if self.selected == 1 {
                        self.username = text;
                    } else {
                        self.password = Zeroizing::new(text);
                    }
                    self.keyboard = None;
                }
            }
            return None;
        }
        if button == Btn::B {
            self.close();
            return Some(AccountEffect::Back);
        }
        if self.account.busy {
            return None;
        }
        match button {
            Btn::Up => self.selected = (self.selected + 4) % 5,
            Btn::Down => self.selected = (self.selected + 1) % 5,
            Btn::A => match self.selected {
                0 => {
                    return Some(AccountEffect::Control(AccountControl::SetEnabled(
                        !self.account.enabled,
                    )))
                }
                1 | 2 if self.account.signed_in => self.status = "Sign out first".into(),
                1 => {
                    self.keyboard = Some(Keyboard::new("RetroAchievements username", 1, 64, false))
                }
                2 => {
                    self.password.zeroize();
                    self.keyboard = Some(Keyboard::secret("RetroAchievements password", 1, 127));
                }
                3 if self.account.signed_in => self.status = "Sign out first".into(),
                3 => {
                    if self.username.trim().is_empty() || self.password.is_empty() {
                        self.status = "Enter username and password".into();
                    } else {
                        let password =
                            std::mem::replace(&mut self.password, Zeroizing::new(String::new()));
                        return Some(AccountEffect::Control(AccountControl::SignIn {
                            username: self.username.clone(),
                            password,
                        }));
                    }
                }
                4 => {
                    self.password.zeroize();
                    return Some(AccountEffect::Control(AccountControl::SignOut));
                }
                _ => {}
            },
            _ => {}
        }
        None
    }
}

impl Drop for AccountScreen {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn screen() -> AccountScreen {
        AccountScreen::new(AccountState::default())
    }
    #[test]
    fn password_entry_is_masked_and_select_reveals() {
        let mut screen = screen();
        screen.selected = 2;
        screen.input(Btn::A);
        screen.input(Btn::A);
        assert_eq!(screen.keyboard.as_ref().unwrap().display(), "*|");
        screen.input(Btn::Select);
        assert_eq!(screen.keyboard.as_ref().unwrap().display(), "a|");
        screen.input(Btn::Start);
        assert_eq!(&**screen.password, "a");
        assert!(screen.keyboard.is_none());
    }
    #[test]
    fn signed_in_edits_are_refused() {
        let mut screen = screen();
        screen.account.signed_in = true;
        for row in [1, 2, 3] {
            screen.selected = row;
            assert!(screen.input(Btn::A).is_none());
            assert!(screen.keyboard.is_none());
            assert_eq!(screen.status, "Sign out first");
        }
    }
    #[test]
    fn a_background_sign_in_closes_an_open_editor_and_clears_secrets() {
        let mut screen = screen();
        screen.selected = 2;
        screen.input(Btn::A);
        screen.input(Btn::A);
        screen.observe(AccountState {
            signed_in: true,
            username: "Player".into(),
            message: "Signed in as Player".into(),
            ..Default::default()
        });
        assert!(screen.keyboard.is_none());
        assert!(screen.password.is_empty());
        assert_eq!(screen.username, "Player");
    }

    #[test]
    fn submit_and_close_empty_the_password_buffer() {
        let mut screen = screen();
        screen.username = "Player".into();
        screen.password = Zeroizing::new("secret".into());
        screen.selected = 3;
        let Some(AccountEffect::Control(AccountControl::SignIn { password, .. })) =
            screen.input(Btn::A)
        else {
            panic!("no submit");
        };
        assert_eq!(&*password, "secret");
        assert!(screen.password.is_empty());
        screen.password = Zeroizing::new("another".into());
        screen.selected = 2;
        screen.input(Btn::A);
        screen.input(Btn::A);
        screen.close();
        assert!(screen.password.is_empty());
        assert!(screen.keyboard.is_none());
    }
}
