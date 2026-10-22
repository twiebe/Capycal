//! Passwords in the platform credential store: the macOS Keychain, or the
//! Secret Service on Linux.

use std::sync::OnceLock;

/// The service of every capycal entry; the entry name is the account's.
pub const SERVICE: &str = "app.capycal";

/// Which store is the default, chosen once per process: the platform's or,
/// in tests, an in-memory one.
static STORE: OnceLock<Store> = OnceLock::new();

#[derive(Debug, PartialEq)]
enum Store {
    Native,
    #[cfg(test)]
    Test,
}

/// Make the platform store the default, unless a store was chosen already.
fn init() {
    STORE.get_or_init(|| {
        if let Err(e) = set_native_store() {
            tracing::debug!(error = %e, "no platform credential store");
        }
        Store::Native
    });
}

#[cfg(target_os = "macos")]
fn set_native_store() -> keyring_core::Result<()> {
    keyring_core::set_default_store(apple_native_keyring_store::keychain::Store::new()?);
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_native_store() -> keyring_core::Result<()> {
    keyring_core::set_default_store(zbus_secret_service_keyring_store::Store::new()?);
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn set_native_store() -> keyring_core::Result<()> {
    Err(keyring_core::Error::NoDefaultStore)
}

/// The password stored under `name`.
pub fn password(name: &str) -> keyring_core::Result<String> {
    init();
    keyring_core::Entry::new(SERVICE, name)?.get_password()
}

/// Store `password` under `name`, replacing an existing one.
pub fn set_password(name: &str, password: &str) -> keyring_core::Result<()> {
    init();
    keyring_core::Entry::new(SERVICE, name)?.set_password(password)
}

/// Remove the entry `name`. A missing entry is fine.
pub fn delete_password(name: &str) -> keyring_core::Result<()> {
    init();
    match keyring_core::Entry::new(SERVICE, name)?.delete_credential() {
        Err(keyring_core::Error::NoEntry) => Ok(()),
        other => other,
    }
}

/// Move the password of `old` to `new` (for a renamed account).
pub fn rename_entry(old: &str, new: &str) -> keyring_core::Result<()> {
    if old == new {
        return Ok(());
    }
    let secret = password(old)?;
    set_password(new, &secret)?;
    delete_password(old)
}

/// Whether reading `name` would make macOS ask for permission: the entry
/// exists but this build of capycal is not on its access list (without a
/// Developer ID every build is a new app to the Keychain). Asks nothing
/// itself.
#[cfg(target_os = "macos")]
pub fn would_prompt(name: &str) -> bool {
    use security_framework::os::macos::keychain::{SecKeychain, SecPreferencesDomain};
    use security_framework::os::macos::passwords::find_generic_password;
    // errSecItemNotFound
    const NOT_FOUND: i32 = -25300;
    let Ok(keychain) = SecKeychain::default_for_domain(SecPreferencesDomain::User) else {
        return false;
    };
    let Ok(_quiet) = SecKeychain::disable_user_interaction() else {
        return false;
    };
    // Without interaction macOS answers with an error where it would ask
    // (errSecInteractionNotAllowed, errSecAuthFailed, a locked keychain)
    match find_generic_password(Some(&[keychain]), SERVICE, name) {
        Ok(_) => false,
        Err(e) => {
            tracing::debug!(
                entry = name,
                code = e.code(),
                "keychain entry not readable silently"
            );
            e.code() != NOT_FOUND
        }
    }
}

/// Other platforms have no per-app access list.
#[cfg(not(target_os = "macos"))]
pub fn would_prompt(_name: &str) -> bool {
    false
}

/// An in-memory store instead of the platform one, for tests. Panics if the
/// platform store was chosen first, so a test never writes to the real one.
#[cfg(test)]
pub(crate) fn use_test_store() {
    let store = STORE.get_or_init(|| {
        keyring_core::set_default_store(keyring_core::sample::Store::new().unwrap());
        Store::Test
    });
    assert_eq!(*store, Store::Test, "the platform store is in use");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_password_reads_the_entry_of_the_name() {
        use_test_store();
        keyring_core::Entry::new(SERVICE, "test-read")
            .unwrap()
            .set_password("s3cret")
            .unwrap();
        assert_eq!(password("test-read").unwrap(), "s3cret");
        assert!(matches!(
            password("test-missing"),
            Err(keyring_core::Error::NoEntry)
        ));
    }

    #[test]
    fn test_set_delete_and_rename_entries() {
        use_test_store();
        set_password("test-set", "one").unwrap();
        set_password("test-set", "two").unwrap();
        assert_eq!(password("test-set").unwrap(), "two");

        rename_entry("test-set", "test-renamed").unwrap();
        assert_eq!(password("test-renamed").unwrap(), "two");
        assert!(matches!(
            password("test-set"),
            Err(keyring_core::Error::NoEntry)
        ));
        assert!(matches!(
            rename_entry("test-set", "x"),
            Err(keyring_core::Error::NoEntry)
        ));

        rename_entry("test-renamed", "test-renamed").unwrap();
        assert_eq!(password("test-renamed").unwrap(), "two");

        delete_password("test-renamed").unwrap();
        delete_password("test-renamed").unwrap();
        assert!(matches!(
            password("test-renamed"),
            Err(keyring_core::Error::NoEntry)
        ));
    }

    /// Reads an entry from the real platform store inside a tokio runtime,
    /// as the daemon does. `make test-keychain` sets the entry first.
    #[tokio::test]
    #[ignore]
    async fn live_password_from_the_platform_store() {
        assert_eq!(password("capycal-test-live").unwrap(), "capycal-test");
    }
}
