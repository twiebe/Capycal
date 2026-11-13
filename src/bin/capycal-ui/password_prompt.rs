//! Asks for the passwords that are missing from the Keychain (macOS) or the
//! keyring (Linux), one account after the other, and stores them there.

use crate::dialog::View;

/// What the app does after an input.
#[derive(Clone, PartialEq, Eq)]
pub enum Reply {
    Close,
    /// Store the password of the account in the keychain; the backend
    /// answers with `Update::PasswordStored`.
    Store {
        account: String,
        password: String,
    },
    /// The view changed; show it again.
    Show,
}

// By hand, so the password never shows (CLAUDE.md "Security")
impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reply::Close => f.write_str("Close"),
            Reply::Store { account, .. } => {
                f.debug_struct("Store").field("account", account).finish()
            }
            Reply::Show => f.write_str("Show"),
        }
    }
}

pub struct PasswordPrompt {
    /// The accounts still without a password, the shown one first.
    accounts: Vec<String>,
    typed: bool,
    saving: bool,
    error: Option<String>,
}

/// What the platform calls the place of the passwords.
pub fn store() -> &'static str {
    if cfg!(target_os = "macos") {
        "Keychain"
    } else {
        "keyring"
    }
}

/// "the account home", "the accounts home, work".
pub fn accounts_text(accounts: &[String]) -> String {
    match accounts {
        [account] => format!("the account {account}"),
        _ => format!("the accounts {}", accounts.join(", ")),
    }
}

impl PasswordPrompt {
    pub fn new(accounts: Vec<String>) -> Self {
        Self {
            accounts,
            typed: false,
            saving: false,
            error: None,
        }
    }

    pub fn typed(&mut self, text: &str) {
        self.typed = !text.is_empty();
    }

    /// A button: 0 is Not Now, 1 is Save with the typed `password`.
    pub fn press(&mut self, button: usize, password: String) -> Reply {
        match (button, self.accounts.first()) {
            (1, Some(account)) if !password.is_empty() && !self.saving => {
                self.saving = true;
                self.error = None;
                Reply::Store {
                    account: account.clone(),
                    password,
                }
            }
            (1, _) => Reply::Show,
            _ => Reply::Close,
        }
    }

    /// The backend stored the password of `account`, or could not.
    pub fn stored(&mut self, account: &str, result: &Result<(), String>) -> Reply {
        if self.accounts.first().map(String::as_str) != Some(account) {
            return Reply::Show;
        }
        self.saving = false;
        match result {
            Ok(()) => {
                self.accounts.remove(0);
                self.typed = false;
                if self.accounts.is_empty() {
                    return Reply::Close;
                }
            }
            Err(e) => self.error = Some(format!("Capycal cannot store the password: {e}")),
        }
        Reply::Show
    }

    pub fn view(&self) -> View {
        let account = self
            .accounts
            .first()
            .map(String::as_str)
            .unwrap_or_default();
        let store = store();
        let why = if cfg!(target_os = "macos") {
            "This happens when the Keychain was reset or the entry was deleted."
        } else {
            "This happens after switching the desktop or the keyring, such as from GNOME to KDE."
        };
        let footnote = match &self.accounts[..] {
            [] | [_] => None,
            [_, next] => Some(format!("Then the password of {next}.")),
            [_, rest @ ..] => Some(format!(
                "Then the passwords of {} more accounts: {}.",
                rest.len(),
                rest.join(", ")
            )),
        };
        View {
            heading: format!("Password of {account}"),
            message: format!(
                "The {store} has no password for the account {account}, so Capycal starts \
                 no syncs. {why} Enter it to store it in the {store} again."
            ),
            lines: Vec::new(),
            buttons: &["Not Now", "Save"],
            links: Vec::new(),
            busy: self.saving,
            destructive: false,
            progress: None,
            choice: None,
            warning: self.error.clone(),
            // A new account gives a new placeholder, which clears the field
            entry: Some(format!("Password of {account}")),
            secret: true,
            last_disabled: !self.typed || self.saving,
            footnote,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> PasswordPrompt {
        PasswordPrompt::new(vec!["home".to_string(), "work".to_string()])
    }

    #[test]
    fn test_save_needs_a_typed_password() {
        let mut p = prompt();
        assert!(p.view().last_disabled);
        assert!(p.view().secret);
        p.typed("s3cret");
        assert!(!p.view().last_disabled);
        assert_eq!(p.press(1, String::new()), Reply::Show);
        assert_eq!(
            p.press(1, "s3cret".to_string()),
            Reply::Store {
                account: "home".to_string(),
                password: "s3cret".to_string()
            }
        );
        // Saving: no second store
        assert!(p.view().busy && p.view().last_disabled);
        assert_eq!(p.press(1, "s3cret".to_string()), Reply::Show);
    }

    #[test]
    fn test_goes_on_with_the_next_account_and_closes_after_the_last() {
        let mut p = prompt();
        assert_eq!(p.view().heading, "Password of home");
        assert_eq!(
            p.view().footnote.as_deref(),
            Some("Then the password of work.")
        );
        p.typed("a");
        p.press(1, "a".to_string());
        assert_eq!(p.stored("home", &Ok(())), Reply::Show);
        let view = p.view();
        assert_eq!(view.heading, "Password of work");
        assert_eq!(view.entry.as_deref(), Some("Password of work"));
        assert!(view.last_disabled && !view.busy);
        assert_eq!(view.footnote, None);
        p.typed("b");
        p.press(1, "b".to_string());
        assert_eq!(p.stored("work", &Ok(())), Reply::Close);
    }

    #[test]
    fn test_a_failed_store_says_why_and_allows_another_try() {
        let mut p = prompt();
        p.typed("a");
        p.press(1, "a".to_string());
        assert_eq!(p.stored("home", &Err("locked".to_string())), Reply::Show);
        let view = p.view();
        assert_eq!(
            view.warning.as_deref(),
            Some("Capycal cannot store the password: locked")
        );
        assert_eq!(view.heading, "Password of home");
        assert!(!view.last_disabled);
    }

    #[test]
    fn test_not_now_closes() {
        assert_eq!(prompt().press(0, String::new()), Reply::Close);
    }

    #[test]
    fn test_names_the_accounts_that_follow() {
        let p = PasswordPrompt::new(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(
            p.view().footnote.as_deref(),
            Some("Then the passwords of 2 more accounts: b, c.")
        );
    }
}
