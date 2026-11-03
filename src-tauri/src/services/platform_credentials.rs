//! Conservative command-safe policy for Sunshine credentials crossing SSH and
//! nested bash -lc boundaries. Reject, never normalize, credentials or secrets.
//! Keep in sync with src/lib/platformCredentials.ts in all three client repos.

pub fn validate_platform_credentials(username: &str, password: &str) -> Result<(), String> {
    if username.len() < 3
        || !username
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !username
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err("Username must have at least 3 characters, start with a letter or number, and use only A-Z, a-z, 0-9, dot, underscore or hyphen.".into());
    }
    if password.len() < 6
        || password.starts_with('-')
        || !password
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._~!@#%^*+=,:?/-".contains(&c))
    {
        return Err("Password must have at least 6 characters, must not start with a hyphen, and use only A-Z, a-z, 0-9 or . _ ~ ! @ # % ^ * + = , : ? / - (no spaces or quotes).".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_command_safe_credentials_without_rewriting() {
        assert!(validate_platform_credentials("Player_1.test-name", "Ab9._~!@#%^*+=,:?/-").is_ok());
    }

    #[test]
    fn checks_every_ascii_character_against_the_allowlists() {
        for byte in 0u8..=127 {
            let username = format!("user{}name", char::from(byte));
            let password = format!("secret{}123", char::from(byte));
            let username_allowed = byte.is_ascii_alphanumeric() || b"._-".contains(&byte);
            let password_allowed =
                byte.is_ascii_alphanumeric() || b"._~!@#%^*+=,:?/-".contains(&byte);
            assert_eq!(
                validate_platform_credentials(&username, "secret123").is_ok(),
                username_allowed
            );
            assert_eq!(
                validate_platform_credentials("player", &password).is_ok(),
                password_allowed
            );
        }
    }

    #[test]
    fn rejects_invalid_usernames() {
        for value in [
            "",
            "ab",
            " user",
            "user ",
            "user\n",
            "-user",
            "_user",
            "us er",
            "us:er",
            "us/er",
            "us'er",
            "us\"er",
            "us$er",
            "us`er",
            "us\\er",
            "us;er",
            "us\0er",
            "usér",
            "user\u{200b}",
        ] {
            assert!(
                validate_platform_credentials(value, "secret123").is_err(),
                "username: {value:?}"
            );
        }
    }

    #[test]
    fn rejects_invalid_passwords_without_exposing_them_in_errors() {
        for value in [
            "",
            "short",
            "-secret",
            " secret",
            "secret ",
            "sec ret",
            "secret\n",
            "secret\r",
            "secret\t",
            "secret\0",
            "sec'ret",
            "sec\"ret",
            "sec\\ret",
            "$(whoami)",
            "`whoami`",
            "secret;id",
            "secret|id",
            "secret&id",
            "secret>out",
            "secret<in",
            "sécret",
            "secret\u{200b}",
        ] {
            let error = validate_platform_credentials("player", value).unwrap_err();
            if value.len() >= 6 {
                assert!(!error.contains(value));
            }
        }
    }
}
