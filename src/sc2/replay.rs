use std::fs;

use nom_mpq::parser;
use serde::Deserialize;

use crate::sc2::error::Sc2Error;

#[derive(Debug, Deserialize)]
struct ReplayMetadataJson {
    #[serde(rename = "BaseBuild", alias = "base_build", alias = "baseBuild")]
    base_build: Option<serde_json::Value>,
    #[serde(rename = "DataVersion", alias = "data_version", alias = "dataVersion")]
    data_version: Option<String>,
}

#[derive(Debug)]
pub struct ReplayMetadata {
    pub base_build: Option<u32>,
    pub data_version: Option<String>,
}

/// Parse the base build and data version out of a replay file's
/// `replay.gamemetadata.json` MPQ sector.
pub fn replay_metadata_from_mpq(replay_path: &str) -> Result<ReplayMetadata, Sc2Error> {
    let replay_bytes = fs::read(replay_path).map_err(|err| {
        Sc2Error::Protocol(format!(
            "failed to read replay {}: {err}",
            replay_path
        ))
    })?;
    let (_, mpq) = parser::parse(&replay_bytes)
        .map_err(|err| Sc2Error::Protocol(format!("failed to parse replay mpq: {err}")))?;
    let (_, metadata_bytes) = mpq
        .read_mpq_file_sector("replay.gamemetadata.json", false, &replay_bytes)
        .map_err(|err| {
            Sc2Error::Protocol(format!(
                "failed to read replay.gamemetadata.json from replay: {err}"
            ))
        })?;

    let metadata: ReplayMetadataJson = serde_json::from_slice(&metadata_bytes).map_err(|err| {
        Sc2Error::Protocol(format!(
            "failed to parse replay metadata json: {err}"
        ))
    })?;
    let base_build = parse_base_build(metadata.base_build.as_ref());
    let data_version = metadata
        .data_version
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    Ok(ReplayMetadata {
        base_build,
        data_version,
    })
}

fn parse_base_build(value: Option<&serde_json::Value>) -> Option<u32> {
    match value {
        Some(serde_json::Value::String(v)) => {
            let v = v.trim();
            if let Some(stripped) = v.strip_prefix("Base") {
                stripped.parse::<u32>().ok()
            } else {
                v.parse::<u32>().ok()
            }
        }
        Some(serde_json::Value::Number(v)) => v.as_u64().and_then(|n| u32::try_from(n).ok()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::replay_metadata_from_mpq;

    #[test]
    fn parses_real_replay_metadata_from_mpq() {
        let replay_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/4656062_tito_puck_MagannathaAIE_v2.SC2Replay");
        let replay_path_str = replay_path
            .to_str()
            .expect("expected replay path to be valid utf-8");

        let metadata = replay_metadata_from_mpq(replay_path_str)
            .expect("expected replay metadata to parse from real replay file");

        assert!(
            metadata.base_build.is_some(),
            "expected BaseBuild in replay metadata"
        );
        assert!(
            metadata
                .data_version
                .as_ref()
                .is_some_and(|v| !v.is_empty()),
            "expected non-empty DataVersion in replay metadata"
        );
    }
}
