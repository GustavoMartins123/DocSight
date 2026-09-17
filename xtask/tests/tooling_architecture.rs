#[allow(dead_code)]
mod support;
use std::fs;
use std::process::Command;
use support::*;
use xtask::architecture::audit;

fn repository() -> TestResult<Fixture> {
    let fixture = Fixture::new()?;
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.root)
        .status()?;
    assert!(status.success());
    Ok(fixture)
}

#[test]
fn clean_rust_checkout_satisfies_the_architecture_gate() -> TestResult {
    let fixture = repository()?;
    fs::write(fixture.root.join("main.rs"), "fn main() {}\n")?;
    assert_eq!(audit(&fixture.root)?["rust_only"], true);
    Ok(())
}
#[test]
fn introduced_scripts_are_detected_even_before_staging() -> TestResult {
    for name in [
        "release.py",
        "other.PY",
        "bytecode.pyc",
        "fallback.pyw",
        "parallel.go",
        "parallel.ts",
        "parallel.js",
    ] {
        let fixture = repository()?;
        fs::write(fixture.root.join(name), b"forbidden implementation")?;
        let report = audit(&fixture.root)?;
        assert_eq!(report["rust_only"], false);
        assert!(
            report["violations"]
                .as_array()
                .ok_or("violations")?
                .iter()
                .any(|value| value == name)
        );
    }
    Ok(())
}
#[test]
fn interpreter_setup_in_ci_is_an_architecture_violation() -> TestResult {
    let fixture = repository()?;
    fs::create_dir_all(fixture.root.join(".github/workflows"))?;
    fs::write(
        fixture.root.join(".github/workflows/test.yml"),
        "steps:\n  - uses: actions/setup-python@v5\n",
    )?;
    assert_eq!(audit(&fixture.root)?["rust_only"], false);
    Ok(())
}
#[test]
fn operational_instructions_cannot_reintroduce_an_interpreter_dependency() -> TestResult {
    let fixture = repository()?;
    fs::write(
        fixture.root.join("README.md"),
        "Run python -m scripts.release\n",
    )?;
    assert_eq!(audit(&fixture.root)?["rust_only"], false);
    Ok(())
}
#[test]
fn validation_gate_does_not_create_new_files() -> TestResult {
    let fixture = repository()?;
    let before = fs::read_dir(&fixture.root)?.count();
    let one = audit(&fixture.root)?;
    assert_eq!(one, audit(&fixture.root)?);
    assert_eq!(before, fs::read_dir(&fixture.root)?.count());
    Ok(())
}
