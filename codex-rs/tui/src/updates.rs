#![cfg(not(debug_assertions))]

use crate::update_action;
use crate::update_action::UpdateAction;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_core::config::Config;
use codex_core::default_client::create_client;
use serde::Deserialize;
use serde::Serialize;
use std::cmp::Ordering;
use std::path::Path;
use std::path::PathBuf;

use crate::version::CODEX_CLI_VERSION;

pub fn get_upgrade_version(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup {
        return None;
    }

    let version_file = version_filepath(config);
    let info = read_version_info(&version_file).ok();

    if match &info {
        None => true,
        Some(info) => info.last_checked_at < Utc::now() - Duration::hours(20),
    } {
        // Refresh the cached latest version in the background so TUI startup
        // isn’t blocked by a network call. The UI reads the previously cached
        // value (if any) for this run; the next run shows the banner if needed.
        tokio::spawn(async move {
            check_for_update(&version_file)
                .await
                .inspect_err(|e| tracing::error!("Failed to update version: {e}"))
        });
    }

    info.and_then(|info| {
        if is_newer(&info.latest_version, CODEX_CLI_VERSION).unwrap_or(false) {
            Some(info.latest_version)
        } else {
            None
        }
    })
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct VersionInfo {
    latest_version: String,
    // ISO-8601 timestamp (RFC3339)
    last_checked_at: DateTime<Utc>,
    #[serde(default)]
    dismissed_version: Option<String>,
}

const VERSION_FILENAME: &str = "co-dex-version.json";
// We use the latest version from the cask if installation is via homebrew - homebrew does not immediately pick up the latest release and can lag behind.
const HOMEBREW_CASK_URL: &str =
    "https://raw.githubusercontent.com/Homebrew/homebrew-cask/HEAD/Casks/c/codex.rb";
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/rosem/codex-weave/releases/latest";

#[derive(Deserialize, Debug, Clone)]
struct ReleaseInfo {
    tag_name: String,
}

fn version_filepath(config: &Config) -> PathBuf {
    config.codex_home.join(VERSION_FILENAME)
}

fn read_version_info(version_file: &Path) -> anyhow::Result<VersionInfo> {
    let contents = std::fs::read_to_string(version_file)?;
    Ok(serde_json::from_str(&contents)?)
}

async fn check_for_update(version_file: &Path) -> anyhow::Result<()> {
    let latest_version = match update_action::get_update_action() {
        Some(UpdateAction::BrewUpgrade) => {
            let cask_contents = create_client()
                .get(HOMEBREW_CASK_URL)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            extract_version_from_cask(&cask_contents)?
        }
        _ => {
            let ReleaseInfo {
                tag_name: latest_tag_name,
            } = create_client()
                .get(LATEST_RELEASE_URL)
                .send()
                .await?
                .error_for_status()?
                .json::<ReleaseInfo>()
                .await?;
            extract_version_from_latest_tag(&latest_tag_name)?
        }
    };

    // Preserve any previously dismissed version if present.
    let prev_info = read_version_info(version_file).ok();
    let info = VersionInfo {
        latest_version,
        last_checked_at: Utc::now(),
        dismissed_version: prev_info.and_then(|p| p.dismissed_version),
    };

    let json_line = format!("{}\n", serde_json::to_string(&info)?);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(version_file, json_line).await?;
    Ok(())
}

fn is_newer(latest: &str, current: &str) -> Option<bool> {
    Some(parse_version(latest)?.cmp(&parse_version(current)?) == Ordering::Greater)
}

fn extract_version_from_cask(cask_contents: &str) -> anyhow::Result<String> {
    cask_contents
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("version \"")
                .and_then(|rest| rest.strip_suffix('"'))
                .map(ToString::to_string)
        })
        .ok_or_else(|| anyhow::anyhow!("Failed to find version in Homebrew cask file"))
}

fn extract_version_from_latest_tag(latest_tag_name: &str) -> anyhow::Result<String> {
    let trimmed = latest_tag_name.trim();
    let trimmed = trimmed
        .strip_prefix("rust-v")
        .or_else(|| trimmed.strip_prefix('v'))
        .unwrap_or(trimmed);
    if !trimmed.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return Err(anyhow::anyhow!(
            "Failed to parse latest tag name '{latest_tag_name}'"
        ));
    }
    Ok(trimmed.to_string())
}

/// Returns the latest version to show in a popup, if it should be shown.
/// This respects the user's dismissal choice for the current latest version.
pub fn get_upgrade_version_for_popup(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup {
        return None;
    }

    let version_file = version_filepath(config);
    let latest = get_upgrade_version(config)?;
    // If the user dismissed this exact version previously, do not show the popup.
    if let Ok(info) = read_version_info(&version_file)
        && info.dismissed_version.as_deref() == Some(latest.as_str())
    {
        return None;
    }
    Some(latest)
}

/// Persist a dismissal for the current latest version so we don't show
/// the update popup again for this version.
pub async fn dismiss_version(config: &Config, version: &str) -> anyhow::Result<()> {
    let version_file = version_filepath(config);
    let mut info = match read_version_info(&version_file) {
        Ok(info) => info,
        Err(_) => return Ok(()),
    };
    info.dismissed_version = Some(version.to_string());
    let json_line = format!("{}\n", serde_json::to_string(&info)?);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(version_file, json_line).await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<Identifier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Identifier {
    Numeric(u64),
    Alpha(String),
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.major.cmp(&other.major) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
        match self.minor.cmp(&other.minor) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
        match self.patch.cmp(&other.patch) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
        compare_pre_release(&self.pre, &other.pre)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_pre_release(left: &[Identifier], right: &[Identifier]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }

    for (left_id, right_id) in left.iter().zip(right.iter()) {
        let ordering = match (left_id, right_id) {
            (Identifier::Numeric(left), Identifier::Numeric(right)) => left.cmp(right),
            (Identifier::Alpha(left), Identifier::Alpha(right)) => left.cmp(right),
            (Identifier::Numeric(_), Identifier::Alpha(_)) => Ordering::Less,
            (Identifier::Alpha(_), Identifier::Numeric(_)) => Ordering::Greater,
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }

    left.len().cmp(&right.len())
}

fn parse_identifier(segment: &str) -> Option<Identifier> {
    if segment.is_empty() {
        return None;
    }
    if segment.chars().all(|ch| ch.is_ascii_digit()) {
        segment.parse::<u64>().ok().map(Identifier::Numeric)
    } else {
        Some(Identifier::Alpha(segment.to_string()))
    }
}

fn parse_version(v: &str) -> Option<Version> {
    let trimmed = v.trim();
    let (without_build, _) = trimmed.split_once('+').unwrap_or((trimmed, ""));
    let (core, pre) = without_build
        .split_once('-')
        .map(|(core, pre)| (core, Some(pre)))
        .unwrap_or((without_build, None));

    let mut iter = core.split('.');
    let major = iter.next()?.parse::<u64>().ok()?;
    let minor = iter.next()?.parse::<u64>().ok()?;
    let patch = iter.next()?.parse::<u64>().ok()?;
    if iter.next().is_some() {
        return None;
    }

    let pre = match pre {
        None => Vec::new(),
        Some(pre) => pre
            .split('.')
            .map(parse_identifier)
            .collect::<Option<Vec<_>>>()?,
    };

    Some(Version {
        major,
        minor,
        patch,
        pre,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_from_cask_contents() {
        let cask = r#"
            cask "codex" do
              version "0.55.0"
            end
        "#;
        assert_eq!(
            extract_version_from_cask(cask).expect("failed to parse version"),
            "0.55.0"
        );
    }

    #[test]
    fn extracts_version_from_latest_tag() {
        assert_eq!(
            extract_version_from_latest_tag("0.91.0-co-dex.1").expect("failed to parse version"),
            "0.91.0-co-dex.1"
        );
        assert_eq!(
            extract_version_from_latest_tag("v1.5.0").expect("failed to parse version"),
            "1.5.0"
        );
    }

    #[test]
    fn latest_tag_without_prefix_is_invalid() {
        assert!(extract_version_from_latest_tag("release-1.5.0").is_err());
    }

    #[test]
    fn prerelease_version_is_not_considered_newer() {
        assert_eq!(is_newer("0.11.0-beta.1", "0.11.0"), Some(false));
        assert_eq!(is_newer("1.0.0-rc.1", "1.0.0"), Some(false));
    }

    #[test]
    fn plain_semver_comparisons_work() {
        assert_eq!(is_newer("0.11.1", "0.11.0"), Some(true));
        assert_eq!(is_newer("0.11.0", "0.11.1"), Some(false));
        assert_eq!(is_newer("1.0.0", "0.9.9"), Some(true));
        assert_eq!(is_newer("0.9.9", "1.0.0"), Some(false));
    }

    #[test]
    fn co_dex_version_comparisons_work() {
        assert_eq!(is_newer("0.91.0-co-dex.2", "0.91.0-co-dex.1"), Some(true));
        assert_eq!(is_newer("0.91.0-co-dex.1", "0.91.0"), Some(false));
    }

    #[test]
    fn whitespace_is_ignored() {
        assert_eq!(
            parse_version(" 1.2.3 \n"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3,
                pre: Vec::new(),
            })
        );
        assert_eq!(is_newer(" 1.2.3 ", "1.2.2"), Some(true));
    }
}
