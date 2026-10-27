use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn canonical_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    // SHA-256 无密钥哈希：64 位 FNV-1a 对蓄意构造者可碰撞，
    // 会导致不同请求被幂等重放或误报冲突。
    let digest = format!("{:x}", Sha256::digest(&bytes));
    format!("v2:{digest}")
}

/// 产物内容的指纹：`sha256-<hex>`。
///
/// 为什么在这里新起一个而不是沿用 `sha1-…`：那个前缀是**历史遗留命名**——`sha1-` 后面
/// 其实是 64 位 FNV-1a（见 `fnv1a_checksum`），对蓄意构造者可碰撞。产物字节是我们**亲手
/// 拿到**的，没有理由再传一份弱哈希；前缀写明算法，下游一眼能看出这条指纹是什么意思。
///
/// 注意：驱动自己报的 checksum（ComfyUI / OpenAI 那两条）仍是 `sha1-…` 老格式，
/// 老数据也不会被改写——`checksum` 这个字段的契约是"**内容指纹**"，不是"某种固定算法"。
pub fn artifact_checksum(bytes: &[u8]) -> String {
    format!("sha256-{:x}", Sha256::digest(bytes))
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
    fn artifact_checksum_is_sha256_and_content_addressed() {
        let digest = artifact_checksum(b"png-bytes");
        assert!(
            digest.starts_with("sha256-"),
            "前缀必须写明算法：下游要能分辨这是哪种指纹（老路径是 sha1- 前缀的 FNV）"
        );
        assert_eq!(
            digest.len(),
            "sha256-".len() + 64,
            "sha256 是 32 字节 = 64 个十六进制字符"
        );
        assert_eq!(
            digest,
            artifact_checksum(b"png-bytes"),
            "同样的字节必须给同样的指纹（否则它就是随机数）"
        );
        assert_ne!(digest, artifact_checksum(b"png-bytes!"));
    }

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
