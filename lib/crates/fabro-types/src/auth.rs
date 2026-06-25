use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IdpIdentityWire", into = "IdpIdentityWire")]
pub struct IdpIdentity {
    issuer: String,
    subject: String,
}

impl IdpIdentity {
    pub fn new(
        issuer: impl Into<String>,
        subject: impl Into<String>,
    ) -> Result<Self, IdpIdentityError> {
        let issuer = issuer.into();
        if issuer.is_empty() {
            return Err(IdpIdentityError::EmptyIssuer);
        }

        let subject = subject.into();
        if subject.is_empty() {
            return Err(IdpIdentityError::EmptySubject);
        }

        Ok(Self { issuer, subject })
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdpIdentityError {
    EmptyIssuer,
    EmptySubject,
}

impl fmt::Display for IdpIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIssuer => f.write_str("IdP issuer must not be empty"),
            Self::EmptySubject => f.write_str("IdP subject must not be empty"),
        }
    }
}

impl std::error::Error for IdpIdentityError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdpIdentityWire {
    issuer: String,
    subject: String,
}

impl TryFrom<IdpIdentityWire> for IdpIdentity {
    type Error = IdpIdentityError;

    fn try_from(value: IdpIdentityWire) -> Result<Self, Self::Error> {
        Self::new(value.issuer, value.subject)
    }
}

impl From<IdpIdentity> for IdpIdentityWire {
    fn from(value: IdpIdentity) -> Self {
        Self {
            issuer: value.issuer,
            subject: value.subject,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IdpIdentity, IdpIdentityError};

    #[test]
    fn constructs_identity_with_non_empty_fields() {
        let identity = IdpIdentity::new("https://github.com", "12345")
            .expect("non-empty fields should construct");

        assert_eq!(identity.issuer(), "https://github.com");
        assert_eq!(identity.subject(), "12345");
    }

    #[test]
    fn rejects_empty_issuer() {
        let err = IdpIdentity::new("", "12345").expect_err("empty issuer should fail");
        assert_eq!(err, IdpIdentityError::EmptyIssuer);
    }

    #[test]
    fn rejects_empty_subject() {
        let err =
            IdpIdentity::new("https://github.com", "").expect_err("empty subject should fail");
        assert_eq!(err, IdpIdentityError::EmptySubject);
    }

    #[test]
    fn serde_rejects_invalid_wire_identity() {
        let err = serde_json::from_str::<IdpIdentity>(r#"{"issuer":"","subject":"12345"}"#)
            .expect_err("serde should reject invalid identity");

        assert!(err.to_string().contains("issuer"));
    }
}
