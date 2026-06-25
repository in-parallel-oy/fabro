//! Per-run ephemeral ed25519 SSH keypair.
//!
//! Generated fresh at `create()`; the public half is injected into the VM via
//! instance metadata (`ssh-keys: <user>:<pubkey>`) and the private half lives
//! **in memory only** for the lifetime of the run — it is never written to
//! disk. When the run ends the VM (and thus the only host that trusts the
//! key) is deleted.

use ssh_key::private::{Ed25519Keypair, Ed25519PrivateKey};
use ssh_key::{LineEnding, PrivateKey};
use zeroize::Zeroizing;

/// An in-memory ephemeral SSH keypair for a single run.
pub struct EphemeralKeypair {
    /// OpenSSH-format private key (`-----BEGIN OPENSSH PRIVATE KEY-----`).
    /// Wrapped in `Zeroizing` so it is wiped from memory on drop.
    private_openssh: Zeroizing<String>,
    /// OpenSSH-format public key line (`ssh-ed25519 AAAA...`).
    public_openssh: String,
}

impl EphemeralKeypair {
    /// Generate a new ed25519 keypair using OS randomness.
    pub fn generate() -> crate::Result<Self> {
        let mut seed = [0u8; 32];
        // rand 0.9 ThreadRng is CSPRNG-backed; the seed never leaves this fn.
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut seed);

        let private = Ed25519PrivateKey::from_bytes(&seed);
        let keypair = Ed25519Keypair::from(private);
        let key = PrivateKey::from(keypair);

        let private_openssh = key.to_openssh(LineEnding::LF).map_err(|err| {
            crate::Error::message(format!("Failed to encode ephemeral private key: {err}"))
        })?;
        let public_openssh = key.public_key().to_openssh().map_err(|err| {
            crate::Error::message(format!("Failed to encode ephemeral public key: {err}"))
        })?;

        Ok(Self {
            private_openssh,
            public_openssh,
        })
    }

    /// The public key line to authorize on the VM, e.g. `ssh-ed25519 AAAA...`.
    #[must_use]
    pub fn public_openssh(&self) -> &str {
        &self.public_openssh
    }

    /// The OpenSSH private key PEM. Callers must keep this in memory only.
    #[must_use]
    pub fn private_openssh(&self) -> &str {
        &self.private_openssh
    }

    /// The `ssh-keys` instance-metadata value authorizing `user` with this
    /// key: `<user>:<pubkey>`.
    #[must_use]
    pub fn ssh_keys_metadata(&self, user: &str) -> String {
        format!("{user}:{}", self.public_openssh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_distinct_ed25519_keys() {
        let a = EphemeralKeypair::generate().unwrap();
        let b = EphemeralKeypair::generate().unwrap();
        assert!(a.public_openssh().starts_with("ssh-ed25519 "));
        assert!(a.private_openssh().contains("OPENSSH PRIVATE KEY"));
        assert_ne!(a.public_openssh(), b.public_openssh());
    }

    #[test]
    fn ssh_keys_metadata_pairs_user_and_key() {
        let key = EphemeralKeypair::generate().unwrap();
        let meta = key.ssh_keys_metadata("fabro");
        assert!(meta.starts_with("fabro:ssh-ed25519 "));
    }
}
