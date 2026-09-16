//! OS keychain broker. The secret never goes in the pack or stdout.

use anyhow::{Context, Result, anyhow};

use super::carrier::Carrier;

const SERVICE: &str = "spoor";

pub fn handle_for(surface_id: &str, carrier: &Carrier) -> String {
    format!("spoor:{surface_id}:{carrier}")
}

pub fn store(handle: &str, secret: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, handle)
        .with_context(|| format!("open keychain entry for handle {handle}"))?;
    entry.set_password(secret).map_err(map_keyring_err)?;
    Ok(())
}

pub fn load(handle: &str) -> Result<String> {
    let entry = keyring::Entry::new(SERVICE, handle)
        .with_context(|| format!("open keychain entry for handle {handle}"))?;
    entry.get_password().map_err(map_keyring_err)
}

pub fn delete(handle: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, handle)
        .with_context(|| format!("open keychain entry for handle {handle}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(map_keyring_err(e)),
    }
}

fn map_keyring_err(err: keyring::Error) -> anyhow::Error {
    match &err {
        keyring::Error::NoEntry => anyhow!(
            "no credential stored in the OS keychain for this handle — run `spoor auth --surface <id>` first"
        ),
        other => {
            let msg = other.to_string();
            let lower = msg.to_ascii_lowercase();
            if lower.contains("denied")
                || lower.contains("not allowed")
                || lower.contains("authorization")
                || lower.contains("user canceled")
                || lower.contains("cancelled")
            {
                anyhow!("keychain access denied: {msg}")
            } else {
                anyhow!("keychain error: {msg}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::carrier::Carrier;

    #[test]
    fn handle_is_stable() {
        let h = handle_for("api-example-test_rest", &Carrier::header("authorization"));
        assert_eq!(h, "spoor:api-example-test_rest:header:authorization");
        let h2 = handle_for("app-example-test_rest", &Carrier::cookie("sid"));
        assert_eq!(h2, "spoor:app-example-test_rest:cookie:sid");
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn keychain_roundtrip() {
        let handle = "spoor:test-handle:header:authorization";
        store(handle, "unit-test-secret-not-real").expect("store");
        let got = load(handle).expect("load");
        assert_eq!(got, "unit-test-secret-not-real");
        delete(handle).expect("delete");
    }
}
