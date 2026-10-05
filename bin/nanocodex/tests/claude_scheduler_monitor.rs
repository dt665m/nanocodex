//! Actual PTY, wall clock, persistence, and process cleanup; only inference is synthetic.
#[test]
fn native_cli_real_clock_schedules_and_monitor_cancellation() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("scripts/tests/claude-scheduler-monitor-cli-journey.py"))
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_nanocodex"))
        .current_dir(&root)
        .output()
        .expect("python3 executes real-clock scheduler and Monitor journey");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}
