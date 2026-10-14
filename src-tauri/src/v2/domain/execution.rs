use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::errors::SafeError;
use super::ids::ArtifactId;
use super::model::LifecycleFeatures;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub vendor_id: String,
    pub protocol_id: String,
    pub display_name: String,
    pub business_capabilities: Vec<String>,
    pub lifecycle_features: LifecycleFeatures,
    pub security_requirements: BTreeMap<String, Value>,
    pub connection_schema: Value,
    pub binding_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHandle {
    pub provider: String,
    pub remote_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactDraft {
    pub filename: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub checksum: String,
    pub data: ArtifactData,
}

impl ArtifactDraft {
    /// 产物名：`{应用名}-{artifactId}.{ext}`。
    ///
    /// **这个名字要回答的是"我在文件夹里怎么认出它"。** 平台给的名字回答不了：
    /// - RunningHub 的回包里**根本没有文件名**（`870.png` 是我们拿输出节点 id 合成的）——
    ///   它比 uuid 好不到哪去，两者都不可辨认；
    /// - 模型 API 那条是 `output.png`，跑两次就分不清；
    /// - ComfyUI 给的是 `ComfyUI_00001_.png`（服务端自己的计数器），只对那台机器有意义。
    ///
    /// 而**唯一能回答"这是哪个应用跑的"的信息只有我们这层有**，所以就用它。
    /// `artifactId` 天生唯一（`run_id*1000+index`）⇒ 顺带把撞名从根上消掉了
    /// （`make_filenames_unique` 因此只在"查不到应用名"的兜底路径上还有活干）。
    ///
    /// ⚠️ **刻意不用 uuid**：人要在文件夹里找东西。主流图像工具没有一个用 uuid——
    /// Midjourney 是「提示词 slug + 短 id」、SD WebUI 是「序号-seed-提示词」、
    /// ComfyUI 是「前缀 + 自增序号」，共识都是"**可读的 + 唯一的**"。
    ///
    /// 扩展名从原名取（png / jpg / mp4 是真的信息，要留着）。应用名里可能带路径分隔符
    /// （用户自己起的名字），落进文件名会变成一个意外的子目录，所以换成 `-`。
    pub fn compose_filename(recipe_name: &str, artifact_id: ArtifactId, original: &str) -> String {
        let recipe_name = recipe_name.trim();
        if recipe_name.is_empty() {
            // 查不到应用名（理论上不该发生）时**退回原名**：不发明，也不丢东西。
            return original.to_string();
        }
        let stem = recipe_name.replace(['/', '\\'], "-");
        match std::path::Path::new(original)
            .extension()
            .map(|value| value.to_string_lossy().into_owned())
        {
            Some(extension) => format!("{stem}-{}.{extension}", artifact_id.0),
            None => format!("{stem}-{}", artifact_id.0),
        }
    }

    /// 保证**同一个 run 内**的文件名互不相同（跨 run 重名是正常的，由落点策略管）。
    ///
    /// 为什么这条属于领域、而不是某个 driver：文件名的**来源**各平台不同，但"一次运行产出的
    /// 东西得能一件件区分开"是执行结果的普遍要求。实测来源：RunningHub 按**输出节点**命名
    /// （`870.png`），同一个节点吐多张图就会撞——而它们在库里是不同 id 的两条记录，
    /// 收进一个目录时人分不清哪张是哪张（`--on-conflict` 只能保证不丢，保证不了能认）。
    ///
    /// 序号插在扩展名前（`870.png` → `870-2.png`），与 `output-0.png` 这个兜底名同形。
    pub fn make_filenames_unique(artifacts: &mut [ArtifactDraft]) {
        let mut taken: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for artifact in artifacts.iter_mut() {
            if taken.insert(artifact.filename.clone()) {
                continue;
            }
            let (stem, extension) = match artifact.filename.rsplit_once('.') {
                Some((stem, extension)) if !stem.is_empty() => {
                    (stem.to_string(), Some(extension.to_string()))
                }
                _ => (artifact.filename.clone(), None),
            };
            let mut index = 2;
            let renamed = loop {
                let candidate = match &extension {
                    Some(extension) => format!("{stem}-{index}.{extension}"),
                    None => format!("{stem}-{index}"),
                };
                if taken.insert(candidate.clone()) {
                    break candidate;
                }
                index += 1;
            };
            artifact.filename = renamed;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactData {
    Bytes(Vec<u8>),
    Url {
        url: String,
        expires_at: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitOutcome {
    Completed(Vec<ArtifactDraft>),
    Accepted(RemoteHandle),
    Uncertain(UncertainSubmission),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncertainSubmission {
    pub reason: String,
    pub recovery: RecoveryProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryProof {
    Idempotent,
    LookupByRequestId(String),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskStatus {
    Created,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub status: TaskStatus,
    pub results: Vec<ArtifactDraft>,
    /// 结构化失败原因（复用 SafeError：code / kind / message / retryable / details）。
    pub error: Option<SafeError>,
    pub usage: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionCheck {
    pub ok: bool,
    pub capabilities: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    Cancelled,
    AlreadyTerminal,
    RemoteMayContinue,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(filename: &str) -> ArtifactDraft {
        ArtifactDraft {
            filename: filename.to_string(),
            mime_type: "image/png".to_string(),
            byte_size: 0,
            checksum: String::new(),
            data: ArtifactData::Url {
                url: "https://example.invalid/a.png".to_string(),
                expires_at: None,
            },
        }
    }

    fn names(artifacts: &[ArtifactDraft]) -> Vec<String> {
        artifacts
            .iter()
            .map(|artifact| artifact.filename.clone())
            .collect()
    }

    /// 产物名的规则：`{应用名}-{artifactId}.{ext}`。
    ///
    /// 这条钉的是"**人能不能在文件夹里认出它**"：平台给的名字（`870.png` 是输出节点 id、
    /// `output.png` 是兜底）都不行，而"哪个应用跑的"只有我们知道。
    #[test]
    fn artifact_names_carry_the_recipe_and_stay_unique_by_construction() {
        let compose = |recipe: &str, id: u64, original: &str| {
            ArtifactDraft::compose_filename(recipe, ArtifactId(id), original)
        };

        // 正常路径：应用名 + artifactId，扩展名留着（png/jpg/mp4 是真信息）。
        assert_eq!(
            compose("Krea2-turbo-文生图", 1000, "870.png"),
            "Krea2-turbo-文生图-1000.png"
        );
        assert_eq!(
            compose("Krea2-turbo-文生图", 1001, "870.png"),
            "Krea2-turbo-文生图-1001.png"
        );
        assert_eq!(compose("My App", 7, "output.mp4"), "My App-7.mp4");

        // ⚠️ 同一批的两张图**同名也没关系**：artifactId 不同 ⇒ 结果不同。
        // 这是"从根上消掉撞名"，不是"撞了再补救"。
        assert_ne!(
            compose("Krea2", 1000, "870.png"),
            compose("Krea2", 1001, "870.png")
        );

        // 应用名里有路径分隔符（用户自己起的名字）不能变成子目录。
        assert_eq!(compose("a/b\\c", 5, "x.png"), "a-b-c-5.png");

        // 查不到应用名 ⇒ **退回原名**，不发明、也不丢东西。
        assert_eq!(compose("", 5, "870.png"), "870.png");
        assert_eq!(compose("   ", 5, "870.png"), "870.png");

        // 原名没有扩展名时也要能拼（`artifact.bin` 这类兜底名）。
        assert_eq!(compose("App", 5, "artifact"), "App-5");
    }

    /// 同一个 run 内必须能一件件区分开：RunningHub 按输出节点命名，一个节点吐多张图就会撞。
    #[test]
    fn filenames_are_unique_within_one_run() {
        // 实测形状：三个产物全叫 `870.png`（同一个输出节点的多张图）。
        let mut artifacts = vec![draft("870.png"), draft("870.png"), draft("870.png")];
        ArtifactDraft::make_filenames_unique(&mut artifacts);
        assert_eq!(names(&artifacts), ["870.png", "870-2.png", "870-3.png"]);

        // 不撞的一个字符都不动（这是主要行为，不是顺带的）。
        let mut untouched = vec![draft("870.png"), draft("871.png"), draft("output.mp4")];
        ArtifactDraft::make_filenames_unique(&mut untouched);
        assert_eq!(names(&untouched), ["870.png", "871.png", "output.mp4"]);

        // 兜底名（无扩展名）也要能消歧，且不能把 `.png` 当成扩展名切错。
        let mut odd = vec![
            draft("artifact"),
            draft("artifact"),
            draft(".png"),
            draft(".png"),
        ];
        ArtifactDraft::make_filenames_unique(&mut odd);
        assert_eq!(names(&odd), ["artifact", "artifact-2", ".png", ".png-2"]);

        // 与"本来就叫 870-2.png"的产物相遇时不能撞上它（序号要继续往后找）。
        let mut clash = vec![draft("870.png"), draft("870-2.png"), draft("870.png")];
        ArtifactDraft::make_filenames_unique(&mut clash);
        assert_eq!(names(&clash), ["870.png", "870-2.png", "870-3.png"]);
    }
}
