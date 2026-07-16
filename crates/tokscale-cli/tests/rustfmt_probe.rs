use std::process::Command;

#[test]
fn formats_workspace_for_coverage_artifact() {
    let status = Command::new("cargo")
        .args(["fmt", "--all"])
        .status()
        .expect("cargo fmt should be available in CI");
    assert!(status.success(), "cargo fmt should succeed");
}
