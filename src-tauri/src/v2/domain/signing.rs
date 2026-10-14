//! 回调签名：HMAC-SHA256 + 时间戳，值形如 `t=<毫秒>,v1=<hex>`。
//!
//! ## 契约（接收端要照着实现的东西）
//!
//! - 请求头：`X-RenderLoft-Signature: t=1791609031058,v1=<hex>`
//! - 被签名的串：`"{t}.{原始请求体}"` —— **请求体的原始字节**，不是重新序列化过的
//! - 算法：HMAC-SHA256，密钥是**人**通过 `callback set-secret` 设进系统凭据库的那一串
//! - 接收端应当：解析 `t` 与 `v1` → 用同一个密钥重算 → 常数时间比较 → 顺带拒绝太旧的 `t`
//!   （时间戳是为了防重放；窗口多大由接收端自己定）
//!
//! 与 Stripe 的 `Stripe-Signature` 同形，原因是这套形状被验证过好用：
//! 时间戳和签名在同一个头里，接收端不用去别处找"签名算的是哪一刻"。
//!
//! ## 为什么自己拼 HMAC 而不用 `hmac` crate
//!
//! 加那个依赖会让 cargo **重新解析整张依赖图**，实测把一个无关的传递依赖顶到没 vendored 的
//! 版本上（`bumpalo 3.20.3`）⇒ 离线构建与 CI 都会红。HMAC 是 RFC 2104 里的标准**构造**
//! （不是需要自己发明的原语），底下用的 `sha2` 本来就是直接依赖，所以这里自己拼，
//! 并用 **RFC 4231 的官方向量**钉住正确性（见下面的测试；向量另用 Python 的 `hmac` 交叉核对过）。
use sha2::{Digest, Sha256};

/// SHA-256 的分组长度（HMAC 的 K' 补齐长度）。
const BLOCK_SIZE: usize = 64;

/// 算 HMAC-SHA256。
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    // RFC 2104：密钥长于分组就先哈希一遍，短了就右侧补零到分组长度。
    let mut padded = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }

    let mut inner = Sha256::new();
    inner.update(padded.map(|byte| byte ^ 0x36));
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(padded.map(|byte| byte ^ 0x5c));
    outer.update(inner_digest);
    outer.finalize().into()
}

/// 生成签名头的值：`t=<毫秒>,v1=<hex>`。
pub fn sign(timestamp_millis: u64, body: &str, key: &[u8]) -> String {
    let signed = format!("{timestamp_millis}.{body}");
    format!(
        "t={timestamp_millis},v1={}",
        hex(&hmac_sha256(key, signed.as_bytes()))
    )
}

/// 小写十六进制。
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_hex(value: &str) -> Vec<u8> {
        (0..value.len() / 2)
            .map(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).expect("hex"))
            .collect()
    }

    /// **RFC 4231 的官方向量**（§4.2–§4.8）。
    ///
    /// 为什么用外部向量而不是自己算一遍存下来：自己算的期望值只能证明"代码没变"，
    /// 证明不了"算对了"。这六条覆盖了三种密钥形态：短于分组、等于分组、长于分组
    /// （长于分组时走"先哈希"那条分支——那条最容易写错）。
    /// 期望值另用 Python 的 `hmac.new(key, data, hashlib.sha256)` 交叉核对过，与 RFC 一致。
    ///
    /// 负对照：把 `hmac_sha256` 里的 `padded.map(|byte| byte ^ 0x36)` 改成不异或
    /// ⇒ 六条全红；把"长于分组先哈希"那段删掉 ⇒ 只有 tc6/tc7 红（正好定位到那条分支）。
    #[test]
    fn hmac_sha256_matches_rfc_4231_vectors() {
        let cases: &[(&str, Vec<u8>, &[u8], &str)] = &[
            (
                "tc1 短密钥",
                vec![0x0b; 20],
                b"Hi There",
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            (
                "tc2 文本密钥",
                b"Jefe".to_vec(),
                b"what do ya want for nothing?",
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ),
            (
                "tc3",
                vec![0xaa; 20],
                &[0xdd; 50],
                "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
            ),
            (
                "tc4 递增密钥",
                (1u8..=25).collect(),
                &[0xcd; 50],
                "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
            ),
            (
                "tc6 超长密钥（走「先哈希」分支）",
                vec![0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First",
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
            (
                "tc7 超长密钥 + 超长消息",
                vec![0xaa; 131],
                b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.",
                "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
            ),
        ];
        for (name, key, message, expected) in cases {
            assert_eq!(
                hex(&hmac_sha256(key, message)),
                *expected,
                "{name} 与 RFC 4231 不符"
            );
        }
    }

    /// 签名头能被接收端**照着契约验回来**——这条就是"接收端"的参考实现。
    ///
    /// 负对照：把 `sign` 里被签名的串从 `{t}.{body}` 改成只签 `{body}`
    /// ⇒ 这里重算时也对不上，测试红。
    #[test]
    fn the_signature_header_verifies_against_the_documented_contract() {
        let key = b"a-shared-secret-only-the-human-knows";
        let body = r#"{"operationId":73,"status":"SUCCEEDED","runs":[{"runId":77}]}"#;
        let header = sign(1_791_609_031_058, body, key);
        assert!(
            header.starts_with("t=1791609031058,v1="),
            "头形状：{header}"
        );

        // ↓ 接收端会做的事
        let (timestamp, signature) = header
            .strip_prefix("t=")
            .and_then(|rest| rest.split_once(",v1="))
            .expect("接收端应当能解析 t 与 v1");
        let recomputed = hex(&hmac_sha256(key, format!("{timestamp}.{body}").as_bytes()));
        assert_eq!(recomputed, signature, "按契约重算应当一致");

        // 换一个密钥、或改动 body 一个字节，都必须对不上（否则签名没有意义）
        assert_ne!(
            hex(&hmac_sha256(
                b"another-key",
                format!("{timestamp}.{body}").as_bytes()
            )),
            signature
        );
        assert_ne!(
            hex(&hmac_sha256(
                key,
                format!("{timestamp}.{}", &body[..body.len() - 1]).as_bytes()
            )),
            signature
        );
    }

    /// 时间戳参与签名：同一份 body、不同时刻，签名必须不同（否则重放挡不住）。
    #[test]
    fn the_timestamp_is_part_of_what_is_signed() {
        let key = b"k";
        let body = "{}";
        let first = sign(1_000, body, key);
        let second = sign(2_000, body, key);
        assert_ne!(first, second);
        assert!(first.starts_with("t=1000,"), "{first}");
        assert!(second.starts_with("t=2000,"), "{second}");
    }

    #[test]
    fn hex_is_lowercase_and_two_digits_per_byte() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(from_hex("000fff"), vec![0x00, 0x0f, 0xff]);
    }
}
