//! The note before macOS asks whether capycal may read its Keychain entries:
//! why it asks, what capycal reads and what it leaves alone.

use crate::dialog::View;

pub fn view(accounts: &[String]) -> View {
    let asks = match accounts {
        [account] => format!("macOS asks once, for the account {account}."),
        _ => format!("macOS asks once per account: {}.", accounts.join(", ")),
    };
    let line = |label: &str, text: &str| (label.to_string(), text.to_string());
    View {
        heading: "Capycal needs the Keychain".to_string(),
        message: "The passwords of your accounts are in the Keychain. This version \
            of Capycal is new to macOS, so macOS asks next whether it may read them."
            .to_string(),
        lines: vec![
            line(
                "Reads",
                "only the passwords Capycal saved, to sign in to your calendar servers",
            ),
            line(
                "Stays closed",
                "every other Keychain item; Capycal never asks for one",
            ),
            line(
                "Sends",
                "passwords to your calendar servers, never into logs or anywhere else",
            ),
        ],
        buttons: &["Continue"],
        links: Vec::new(),
        busy: false,
        destructive: false,
        progress: None,
        choice: None,
        warning: None,
        entry: None,
        last_disabled: false,
        footnote: Some(format!(
            "{asks} Choose Always Allow, so it does not ask again."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_names_the_accounts_macos_asks_about() {
        let one = view(&["work".to_string()]);
        assert_eq!(
            one.footnote.as_deref(),
            Some(
                "macOS asks once, for the account work. Choose Always Allow, so it does not ask again."
            )
        );
        let two = view(&["home".to_string(), "work".to_string()]);
        assert!(
            two.footnote
                .unwrap()
                .starts_with("macOS asks once per account: home, work.")
        );
        assert_eq!(two.buttons, &["Continue"]);
    }
}
