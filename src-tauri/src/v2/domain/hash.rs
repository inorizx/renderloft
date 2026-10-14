use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn canonical_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    // SHA-256 无密钥哈希：64 位 FNV-1a 对蓄意构造者可碰撞，
    // 会导致不同请求被幂等重放或误报冲突。
    let digest = format!("{:x}", Sha256::digest(&bytes));
    format!("v2:{digest}")
}

/// FNV-1a 64 位校验和（资产校验和；`sha1-` 前缀为历史遗留命名）。
pub fn fnv1a_checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_is_deterministic_for_equal_values() {
        let a = canonical_json(&json!({ "b": 1, "a": [1, 2] }));
        let b = canonical_json(&json!({ "a": [1, 2], "b": 1 }));
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_json_differs_for_different_values() {
        let a = canonical_json(&json!({ "prompt": "hello" }));
        let b = canonical_json(&json!({ "prompt": "world" }));
        assert_ne!(a, b);
    }
}
