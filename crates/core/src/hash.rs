use sha2::{Digest, Sha256};

/// Computes a deterministic SHA-256 hash over backup content bytes.
///
/// M1 intentionally hashes a single collection payload. Future milestones can
/// expand this to canonicalized collection + media signatures.
pub fn content_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::content_hash;

    #[test]
    fn hash_is_stable_for_same_content() {
        let data = b"anki-backup-test";
        let left = content_hash(data);
        let right = content_hash(data);
        assert_eq!(left, right);
    }

    #[test]
    fn hash_changes_when_content_changes() {
        let one = content_hash(b"v1");
        let two = content_hash(b"v2");
        assert_ne!(one, two);
    }
}
