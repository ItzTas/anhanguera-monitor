use std::fmt;

use zeroize::Zeroizing;

const REDACTED: &str = "[redacted]";

#[derive(Clone, Default)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Secret {
        Secret(Zeroizing::new(value))
    }

    pub fn reveal(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_prints_its_value() {
        let secret = Secret::new("hunter2".to_owned());

        assert_eq!(format!("{:?} {}", secret, secret), "[redacted] [redacted]");
        assert_eq!(secret.reveal(), "hunter2");
    }

    #[test]
    fn empty_secret_is_empty() {
        assert!(Secret::default().is_empty());
        assert!(!Secret::new("x".to_owned()).is_empty());
    }
}
