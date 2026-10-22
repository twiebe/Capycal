//! The account form of the Settings window: server, user name and a
//! password kept in the keychain.

use capycal::{AccountConfig, Credential};

#[derive(Clone, PartialEq, Eq)]
pub enum Password {
    /// Leave the stored credential as it is.
    Keep,
    New(String),
}

/// A change of the keychain that goes with saving an account.
#[derive(Clone, PartialEq, Eq)]
pub enum KeychainOp {
    Set { name: String, password: String },
    Rename { old: String, new: String },
    Delete { name: String },
}

// Debug never shows a password (CLAUDE.md "Security")
impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Password::Keep => f.write_str("Keep"),
            Password::New(_) => f.write_str("New(***)"),
        }
    }
}

impl std::fmt::Debug for KeychainOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeychainOp::Set { name, .. } => write!(f, "Set {{ name: {name:?}, password: *** }}"),
            KeychainOp::Rename { old, new } => write!(f, "Rename {{ old: {old:?}, new: {new:?} }}"),
            KeychainOp::Delete { name } => write!(f, "Delete {{ name: {name:?} }}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccountForm {
    original: Option<(String, AccountConfig)>,
    name: String,
    url: String,
    username: String,
    password: Password,
}

impl AccountForm {
    pub fn new_account() -> Self {
        Self {
            original: None,
            name: String::new(),
            url: String::new(),
            username: String::new(),
            password: Password::Keep,
        }
    }

    pub fn edit(name: &str, account: &AccountConfig) -> Self {
        Self {
            original: Some((name.to_string(), account.clone())),
            name: name.to_string(),
            url: account.url.clone(),
            username: account.username.clone(),
            password: Password::Keep,
        }
    }

    pub fn original_name(&self) -> Option<&str> {
        self.original.as_ref().map(|(n, _)| n.as_str())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn set_name(&mut self, text: &str) {
        self.name = text.trim().to_string();
    }

    pub fn set_url(&mut self, text: &str) {
        self.url = text.trim().to_string();
    }

    pub fn set_username(&mut self, text: &str) {
        self.username = text.trim().to_string();
    }

    /// The typed password, empty when the stored one stays.
    pub fn password_text(&self) -> &str {
        match &self.password {
            Password::New(p) => p,
            Password::Keep => "",
        }
    }

    /// An empty text keeps the stored password.
    pub fn set_password(&mut self, text: &str) {
        self.password = match text {
            "" => Password::Keep,
            text => Password::New(text.to_string()),
        };
    }

    /// Where the password comes from when it is not the keychain entry the
    /// app manages; the form then shows it read-only until a new one is typed.
    pub fn managed_externally(&self) -> Option<&'static str> {
        match &self.original.as_ref()?.1.password {
            Credential::Env { .. } => Some("environment variable"),
            Credential::File { .. } => Some("file"),
            Credential::Command { .. } => Some("command"),
            Credential::Plain(_) => Some("config file"),
            Credential::Keychain { .. } => None,
        }
    }

    pub fn is_dirty(&self) -> bool {
        match &self.original {
            None => {
                !self.name.is_empty()
                    || !self.url.is_empty()
                    || !self.username.is_empty()
                    || self.password != Password::Keep
            }
            Some((name, account)) => {
                *name != self.name
                    || account.url != self.url
                    || account.username != self.username
                    || self.password != Password::Keep
            }
        }
    }

    /// The name and account to write, the keychain changes and the old name
    /// if the account was renamed. `other_names` are the other accounts.
    pub fn build(
        &self,
        other_names: &[String],
    ) -> Result<(String, AccountConfig, Vec<KeychainOp>, Option<String>), String> {
        let name = &self.name;
        if name.is_empty() {
            return Err("Enter a name".to_string());
        }
        if name.contains('/') || name.contains('\\') || name.starts_with('.') {
            return Err("No slashes and no leading dot".to_string());
        }
        if other_names.contains(name) {
            return Err(format!("An account named \"{name}\" exists already"));
        }
        let url_ok =
            reqwest::Url::parse(&self.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
        if !url_ok {
            return Err("Enter a URL starting with https:// or http://".to_string());
        }
        if self.username.is_empty() {
            return Err("Enter the user name".to_string());
        }
        let keychain = |n: &str| Credential::Keychain {
            keychain: n.to_string(),
        };
        let renamed = self
            .original
            .as_ref()
            .map(|(old, _)| old.clone())
            .filter(|old| old != name);
        let (password, ops) = match (&self.password, &self.original) {
            (Password::New(password), original) => {
                let mut ops = vec![KeychainOp::Set {
                    name: name.clone(),
                    password: password.clone(),
                }];
                if let (Some(old), Some((_, account))) = (&renamed, original)
                    && account.password == keychain(old)
                {
                    ops.push(KeychainOp::Delete { name: old.clone() });
                }
                (keychain(name), ops)
            }
            (Password::Keep, None) => return Err("Enter the password".to_string()),
            (Password::Keep, Some((_, account))) => match &renamed {
                Some(old) if account.password == keychain(old) => (
                    keychain(name),
                    vec![KeychainOp::Rename {
                        old: old.clone(),
                        new: name.clone(),
                    }],
                ),
                _ => (account.password.clone(), Vec::new()),
            },
        };
        let account = AccountConfig {
            url: self.url.clone(),
            username: self.username.clone(),
            password,
        };
        Ok((name.clone(), account, ops, renamed))
    }

    /// URL, user name and the typed password (None: use the stored one),
    /// for "Test".
    pub fn test_credentials(&self) -> (String, String, Option<String>) {
        let password = match &self.password {
            Password::New(p) => Some(p.clone()),
            Password::Keep => None,
        };
        (self.url.clone(), self.username.clone(), password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capycal::{AccountConfig, Credential};

    fn keychain_account() -> AccountConfig {
        AccountConfig {
            url: "https://x/".into(),
            username: "u".into(),
            password: Credential::Keychain {
                keychain: "work".into(),
            },
        }
    }

    #[test]
    fn test_new_account_stores_the_password_in_the_keychain() {
        let mut form = AccountForm::new_account();
        form.set_name("nextcloud");
        form.set_url("https://cloud.example.org/");
        form.set_username("alice");
        form.set_password("s3cret");
        let (name, account, ops, renamed) = form.build(&[]).unwrap();
        assert_eq!(name, "nextcloud");
        assert_eq!(
            account.password,
            Credential::Keychain {
                keychain: "nextcloud".into()
            }
        );
        assert_eq!(
            ops,
            [KeychainOp::Set {
                name: "nextcloud".into(),
                password: "s3cret".into()
            }]
        );
        assert_eq!(renamed, None);
        let mut missing = AccountForm::new_account();
        missing.set_name("x");
        missing.set_url("https://x/");
        missing.set_username("u");
        assert_eq!(missing.build(&[]).unwrap_err(), "Enter the password");
    }

    #[test]
    fn test_rename_moves_the_keychain_entry() {
        let mut form = AccountForm::edit("work", &keychain_account());
        assert!(!form.is_dirty());
        form.set_name("office");
        let (_, account, ops, renamed) = form.build(&[]).unwrap();
        assert_eq!(
            account.password,
            Credential::Keychain {
                keychain: "office".into()
            }
        );
        assert_eq!(
            ops,
            [KeychainOp::Rename {
                old: "work".into(),
                new: "office".into()
            }]
        );
        assert_eq!(renamed.as_deref(), Some("work"));
        form.set_password("new");
        let (_, _, ops, _) = form.build(&[]).unwrap();
        assert_eq!(
            ops,
            [
                KeychainOp::Set {
                    name: "office".into(),
                    password: "new".into()
                },
                KeychainOp::Delete {
                    name: "work".into()
                },
            ]
        );
    }

    #[test]
    fn test_external_credentials_stay_untouched_until_a_password_is_typed() {
        let env = AccountConfig {
            password: Credential::Env { env: "PW".into() },
            ..keychain_account()
        };
        let mut form = AccountForm::edit("work", &env);
        assert_eq!(form.managed_externally(), Some("environment variable"));
        form.set_url("https://y/");
        let (_, account, ops, _) = form.build(&[]).unwrap();
        assert_eq!(account.password, Credential::Env { env: "PW".into() });
        assert!(ops.is_empty());
        form.set_password("typed");
        let (_, account, ops, _) = form.build(&[]).unwrap();
        assert_eq!(
            account.password,
            Credential::Keychain {
                keychain: "work".into()
            }
        );
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn test_account_validation() {
        let mut form = AccountForm::new_account();
        assert_eq!(form.build(&[]).unwrap_err(), "Enter a name");
        form.set_name("acc");
        form.set_url("not a url");
        form.set_username("u");
        form.set_password("p");
        assert_eq!(
            form.build(&["acc".into()]).unwrap_err(),
            "An account named \"acc\" exists already"
        );
        assert_eq!(
            form.build(&[]).unwrap_err(),
            "Enter a URL starting with https:// or http://"
        );
    }

    #[test]
    fn test_debug_hides_passwords() {
        let mut form = AccountForm::new_account();
        form.set_password("hunter2");
        let op = KeychainOp::Set {
            name: "a".into(),
            password: "hunter2".into(),
        };
        assert!(!format!("{form:?}{op:?}").contains("hunter2"));
    }
}
