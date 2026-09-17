#[allow(dead_code)]
mod support;
use std::process::Command;
use support::*;
use xtask::release::changelog::{generate, render};
use xtask::tooling::common::workspace_version;

#[test]
fn conventional_changes_are_grouped_without_losing_git_order() -> TestResult {
    let records = [
        "feat(parser): add feature",
        "fix(parser): correct error",
        "perf: accelerate",
        "test: add case",
        "docs: describe",
        "ci: test",
        "miscellaneous",
    ]
    .iter()
    .enumerate()
    .map(|(index, subject)| (format!("{:040x}", index + 1), (*subject).into()))
    .collect::<Vec<_>>();
    let notes = render(&records, "1.2.3", REVISION)?;
    for (_, subject) in &records {
        assert!(
            notes.contains(
                subject
                    .split(':')
                    .next()
                    .ok_or("subject")?
                    .split('(')
                    .next()
                    .ok_or("kind")?
            )
        );
    }
    assert!(notes.find("Features") < notes.find("Fixes"));
    assert_eq!(notes, render(&records, "1.2.3", REVISION)?);
    Ok(())
}
#[test]
fn changelog_escapes_markdown_and_validates_identity() -> TestResult {
    let notes = render(
        &[(REVISION.into(), "fix: [bad](url) <script> _x_".into())],
        "1.0.0",
        REVISION,
    )?;
    assert!(notes.contains("\\[bad\\]\\(url\\)"));
    assert!(notes.contains("\\<script\\>"));
    assert!(render(&[], "v1.0.0", REVISION).is_err());
    assert!(render(&[], "1.0.0", "main").is_err());
    assert!(render(&[("bad".into(), "fix: x".into())], "1.0.0", REVISION).is_err());
    Ok(())
}
#[test]
fn changelog_rejects_multiline_empty_and_unbounded_subjects() {
    for subject in [
        String::new(),
        "fix: first\nsecond".into(),
        "fix: x\0private".into(),
        "a".repeat(8193),
    ] {
        assert!(render(&[(REVISION.into(), subject)], "1.0.0", REVISION).is_err());
    }
}
#[test]
fn empty_changelog_is_explicit_and_deterministic() -> TestResult {
    let notes = render(&[], "1.0.0", REVISION)?;
    assert!(notes.contains("No non-merge commits"));
    assert!(notes.ends_with('\n'));
    Ok(())
}
fn git(root: &std::path::Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err("Test Git command failed".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
#[test]
fn real_git_range_is_verified_before_changelog_generation() -> TestResult {
    let fixture = Fixture::new()?;
    git(&fixture.root, &["init", "--quiet"])?;
    git(&fixture.root, &["config", "user.name", "Synthetic Test"])?;
    git(
        &fixture.root,
        &["config", "user.email", "test@example.invalid"],
    )?;
    git(
        &fixture.root,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "docs: base",
        ],
    )?;
    let base = git(&fixture.root, &["rev-parse", "HEAD"])?;
    git(
        &fixture.root,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "feat: added",
        ],
    )?;
    let head = git(&fixture.root, &["rev-parse", "HEAD"])?;
    let notes = generate(&fixture.root, workspace_version(), &head, Some(&base))?;
    assert!(notes.contains("feat: added"));
    assert!(!notes.contains("docs: base"));
    assert!(generate(&fixture.root, workspace_version(), &base, Some(&head)).is_err());
    Ok(())
}
