//! HuggingFace 仓库文件分类模块
//!
//! 根据文件名/后缀将仓库文件分类为模型权重、配置、Tokenizer、代码、数据、文档等类别。

use serde::{Deserialize, Serialize};

/// 文件分类枚举
///
/// 序列化为 camelCase 以与前端 `FileCategory` 类型对齐
/// (前端 `types.ts`: `'modelWeight' | 'config' | ...`)。
/// 缺失 `rename_all` 会导致 PascalCase 序列化(`"ModelWeight"`),
/// 前端分类匹配失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileCategory {
    /// 模型权重文件
    ModelWeight,
    /// 配置文件
    Config,
    /// Tokenizer 文件
    Tokenizer,
    /// 代码文件
    Code,
    /// 数据文件
    Data,
    /// 文档文件
    Document,
    /// 其他未分类文件
    Other,
}

/// 根据文件路径判断其分类
///
/// 判断优先级: 特定文件名(完整匹配,大小写不敏感) > 特定后缀(大小写不敏感) > Other
pub fn classify_file(path: &str) -> FileCategory {
    let lower = path.to_lowercase();
    let file_name = lower
        .rsplit_once('/')
        .map_or(lower.as_str(), |(_, name)| name);

    // 1. 特定文件名完整匹配 — 模型权重
    if matches!(
        file_name,
        "pytorch_model.bin" | "tf_model.h5" | "model.safetensors" | "rust_model.ot"
    ) {
        return FileCategory::ModelWeight;
    }

    // 2. 特定文件名完整匹配 — 配置
    if matches!(
        file_name,
        "config.json"
            | "generation_config.json"
            | "preprocessor_config.json"
            | "feature_extractor_config.json"
            | "processing_config.json"
    ) {
        return FileCategory::Config;
    }

    // 3. 特定文件名完整匹配 — Tokenizer
    if matches!(
        file_name,
        "tokenizer.json"
            | "tokenizer_config.json"
            | "tokenizer.model"
            | "special_tokens_map.json"
            | "vocab.txt"
            | "merges.txt"
    ) {
        return FileCategory::Tokenizer;
    }

    // 4. 特定文件名完整匹配 — 文档
    if matches!(
        file_name,
        "readme.md" | "readme.rst" | "readme.txt" | "license" | "license.md"
    ) {
        return FileCategory::Document;
    }

    // 5. 后缀匹配
    let suffix = lower.rsplit_once('.').map_or("", |(_, ext)| ext);

    match suffix {
        "safetensors" | "bin" | "onnx" | "gguf" | "pt" | "pth" | "h5" | "msgpack" | "ot" => {
            FileCategory::ModelWeight
        }
        "py" | "cpp" | "c" | "h" | "sh" | "js" => FileCategory::Code,
        "csv" | "jsonl" | "parquet" | "arrow" | "tsv" => FileCategory::Data,
        "md" | "rst" | "txt" => FileCategory::Document,
        _ => FileCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_weight_by_suffix() {
        assert_eq!(
            classify_file("model.safetensors"),
            FileCategory::ModelWeight
        );
        assert_eq!(classify_file("model.bin"), FileCategory::ModelWeight);
        assert_eq!(classify_file("model.onnx"), FileCategory::ModelWeight);
        assert_eq!(classify_file("model.gguf"), FileCategory::ModelWeight);
        assert_eq!(classify_file("model.pt"), FileCategory::ModelWeight);
        assert_eq!(classify_file("model.pth"), FileCategory::ModelWeight);
        assert_eq!(classify_file("model.h5"), FileCategory::ModelWeight);
        assert_eq!(classify_file("model.msgpack"), FileCategory::ModelWeight);
    }

    #[test]
    fn test_model_weight_by_exact_name() {
        assert_eq!(
            classify_file("pytorch_model.bin"),
            FileCategory::ModelWeight
        );
        assert_eq!(classify_file("tf_model.h5"), FileCategory::ModelWeight);
        assert_eq!(
            classify_file("model.safetensors"),
            FileCategory::ModelWeight
        );
        assert_eq!(classify_file("rust_model.ot"), FileCategory::ModelWeight);
    }

    #[test]
    fn test_config_files() {
        assert_eq!(classify_file("config.json"), FileCategory::Config);
        assert_eq!(
            classify_file("generation_config.json"),
            FileCategory::Config
        );
        assert_eq!(
            classify_file("preprocessor_config.json"),
            FileCategory::Config
        );
        assert_eq!(
            classify_file("feature_extractor_config.json"),
            FileCategory::Config
        );
        assert_eq!(
            classify_file("processing_config.json"),
            FileCategory::Config
        );
    }

    #[test]
    fn test_tokenizer_files() {
        assert_eq!(classify_file("tokenizer.json"), FileCategory::Tokenizer);
        assert_eq!(
            classify_file("tokenizer_config.json"),
            FileCategory::Tokenizer
        );
        assert_eq!(classify_file("tokenizer.model"), FileCategory::Tokenizer);
        assert_eq!(
            classify_file("special_tokens_map.json"),
            FileCategory::Tokenizer
        );
        assert_eq!(classify_file("vocab.txt"), FileCategory::Tokenizer);
        assert_eq!(classify_file("merges.txt"), FileCategory::Tokenizer);
    }

    #[test]
    fn test_code_files() {
        assert_eq!(classify_file("inference.py"), FileCategory::Code);
        assert_eq!(classify_file("model.cpp"), FileCategory::Code);
        assert_eq!(classify_file("kernel.c"), FileCategory::Code);
        assert_eq!(classify_file("header.h"), FileCategory::Code);
        assert_eq!(classify_file("setup.sh"), FileCategory::Code);
        assert_eq!(classify_file("script.js"), FileCategory::Code);
    }

    #[test]
    fn test_data_files() {
        assert_eq!(classify_file("train.csv"), FileCategory::Data);
        assert_eq!(classify_file("train.jsonl"), FileCategory::Data);
        assert_eq!(classify_file("data.parquet"), FileCategory::Data);
        assert_eq!(classify_file("data.arrow"), FileCategory::Data);
        assert_eq!(classify_file("train.tsv"), FileCategory::Data);
    }

    #[test]
    fn test_document_files() {
        assert_eq!(classify_file("README.md"), FileCategory::Document);
        assert_eq!(classify_file("README.rst"), FileCategory::Document);
        assert_eq!(classify_file("README.txt"), FileCategory::Document);
        assert_eq!(classify_file("LICENSE"), FileCategory::Document);
        assert_eq!(classify_file("LICENSE.md"), FileCategory::Document);
        assert_eq!(classify_file("guide.md"), FileCategory::Document);
        assert_eq!(classify_file("guide.rst"), FileCategory::Document);
        assert_eq!(classify_file("notes.txt"), FileCategory::Document);
    }

    #[test]
    fn test_other_files() {
        assert_eq!(classify_file("unknown.xyz"), FileCategory::Other);
        assert_eq!(classify_file("data.zip"), FileCategory::Other);
        assert_eq!(classify_file("noextension"), FileCategory::Other);
    }

    #[test]
    fn test_nested_path() {
        assert_eq!(
            classify_file("subdir/model.safetensors"),
            FileCategory::ModelWeight
        );
        assert_eq!(
            classify_file("deep/nested/path/config.json"),
            FileCategory::Config
        );
        assert_eq!(
            classify_file("models/bert/tokenizer.json"),
            FileCategory::Tokenizer
        );
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(
            classify_file("MODEL.SAFETENSORS"),
            FileCategory::ModelWeight
        );
        assert_eq!(classify_file("Config.JSON"), FileCategory::Config);
        assert_eq!(classify_file("Tokenizer.JSON"), FileCategory::Tokenizer);
        assert_eq!(classify_file("README.MD"), FileCategory::Document);
        assert_eq!(classify_file("script.PY"), FileCategory::Code);
    }

    /// 锁定 wire format:FileCategory 必须序列化为 camelCase,
    /// 与前端 `types.ts` 的 `'modelWeight' | 'config' | ...` 对齐。
    /// 防止缺失 `#[serde(rename_all = "camelCase")]` 导致 PascalCase 失配。
    #[test]
    fn test_file_category_serializes_camel_case() {
        let cases = [
            (FileCategory::ModelWeight, "\"modelWeight\""),
            (FileCategory::Config, "\"config\""),
            (FileCategory::Tokenizer, "\"tokenizer\""),
            (FileCategory::Code, "\"code\""),
            (FileCategory::Data, "\"data\""),
            (FileCategory::Document, "\"document\""),
            (FileCategory::Other, "\"other\""),
        ];
        for (variant, expected_json) in cases {
            let serialized = serde_json::to_string(&variant).expect("序列化失败");
            assert_eq!(
                serialized, expected_json,
                "FileCategory::{variant:?} 序列化为 {serialized}, 期望 {expected_json}(camelCase)"
            );
            // 往返:反序列化必须还原
            let deserialized: FileCategory =
                serde_json::from_str(expected_json).expect("反序列化失败");
            assert_eq!(deserialized, variant, "反序列化 {expected_json} 失配");
        }
    }
}
