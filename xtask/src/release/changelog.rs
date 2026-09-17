use crate::tooling::{
    common::*,
    process::{ProcessLimits, run_bounded},
};
use std::path::Path;

pub const GROUPS: [&str; 7] = [
    "Features",
    "Fixes",
    "Performance",
    "Tests",
    "Documentation",
    "Build and maintenance",
    "Other",
];

fn group(subject: &str) -> usize {
    let Some((prefix, body)) = subject.split_once(':') else {
        return 6;
    };
    if !body.starts_with(char::is_whitespace) {
        return 6;
    }
    let prefix = prefix.trim_end_matches('!');
    let kind = if let Some((kind, scope)) = prefix.split_once('(') {
        if !scope.ends_with(')') || scope[..scope.len() - 1].contains(')') {
            return 6;
        }
        kind
    } else {
        prefix
    };
    match kind {
        "feat" => 0,
        "fix" => 1,
        "perf" => 2,
        "test" => 3,
        "docs" => 4,
        "build" | "ci" | "chore" | "refactor" => 5,
        _ => 6,
    }
}

pub fn render(records: &[(String, String)], version: &str, revision: &str) -> Result<String> {
    checked_version(version)?;
    checked_revision(revision)?;
    let mut grouped: [Vec<String>; 7] = std::array::from_fn(|_| Vec::new());
    for (sha, subject) in records {
        checked_revision(sha)?;
        require(
            !subject.is_empty() && subject.len() <= 8192 && !subject.contains(['\n', '\r', '\0']),
            "INVALID_COMMIT_SUBJECT",
            "Commit subjects must be bounded nonempty single lines",
        )?;
        let mut escaped = String::new();
        for character in subject.chars() {
            if "\\`*{}[]()<>_#".contains(character) {
                escaped.push('\\');
            }
            escaped.push(character);
        }
        grouped[group(subject)].push(format!("- {escaped} ({})", &sha[..12]));
    }
    let mut output = format!(
        "# DocSight {version}\n\nCandidate commit: `{revision}`.\n\nGenerated from non-merge Git commits. Publishing requires the release validation gates.\n\n"
    );
    for (name, entries) in GROUPS.into_iter().zip(grouped) {
        if !entries.is_empty() {
            output.push_str(&format!("## {name}\n\n{}\n\n", entries.join("\n")));
        }
    }
    if records.is_empty() {
        output.push_str("No non-merge commits exist in the requested range.\n");
    } else {
        output.pop();
    }
    Ok(output)
}

pub fn generate(root: &Path, version: &str, revision: &str, since: Option<&str>) -> Result<String> {
    checked_revision(revision)?;
    if let Some(since) = since {
        checked_revision(since)?;
        let result = run_bounded(
            &["git", "merge-base", "--is-ancestor", since, revision],
            root,
            &ProcessLimits::default(),
            None,
        )?;
        require(
            result.returncode == 0 && result.termination.is_none(),
            "INVALID_CHANGELOG_RANGE",
            "Previous revision must be an ancestor of the candidate",
        )?;
    }
    let selection = since
        .map(|since| format!("{since}..{revision}"))
        .unwrap_or_else(|| revision.to_owned());
    let result = run_bounded(
        &[
            "git",
            "log",
            "--reverse",
            "--no-merges",
            "--format=%H%x00%s",
            &selection,
            "--",
        ],
        root,
        &ProcessLimits {
            output_bytes: 2_097_152,
            ..ProcessLimits::default()
        },
        None,
    )?;
    require(
        result.returncode == 0 && result.termination.is_none(),
        "GIT_HISTORY_UNAVAILABLE",
        "Cannot read requested bounded Git history",
    )?;
    let mut records = Vec::new();
    for line in text(&result.stdout)?.lines() {
        let (sha, subject) = line.split_once('\0').ok_or_else(|| {
            ToolError::new(
                "INVALID_GIT_OUTPUT",
                "Git history does not match its requested format",
            )
        })?;
        records.push((sha.to_owned(), subject.to_owned()));
    }
    render(&records, version, revision)
}
