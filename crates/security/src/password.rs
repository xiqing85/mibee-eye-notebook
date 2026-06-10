use anyhow::Result;

/// Hash a plaintext password using bcrypt.
///
/// Uses the default bcrypt cost (12) for a good balance of security and speed.
pub fn hash_password(plain: &str) -> Result<String> {
    Ok(bcrypt::hash(plain, bcrypt::DEFAULT_COST)?)
}

/// Verify a plaintext password against a bcrypt hash.
pub fn verify_password(plain: &str, hash: &str) -> Result<bool> {
    Ok(bcrypt::verify(plain, hash)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_and_verify_roundtrip() {
        let password = "hunter2";
        let hash = hash_password(password).unwrap();
        assert!(verify_password(password, &hash).unwrap());
    }

    #[test]
    fn test_verify_wrong_password() {
        let hash = hash_password("correct_password").unwrap();
        assert!(!verify_password("wrong_password", &hash).unwrap());
    }

    #[test]
    fn test_same_password_different_hashes() {
        let pwd = "password123";
        let h1 = hash_password(pwd).unwrap();
        let h2 = hash_password(pwd).unwrap();
        // bcrypt uses random salts, so hashes should differ
        assert_ne!(h1, h2);
        // Both should verify correctly
        assert!(verify_password(pwd, &h1).unwrap());
        assert!(verify_password(pwd, &h2).unwrap());
    }

    #[test]
    fn test_empty_password() {
        let hash = hash_password("").unwrap();
        assert!(verify_password("", &hash).unwrap());
        assert!(!verify_password(" ", &hash).unwrap());
    }
}
