//! Public durable-session recovery journey across real process death and SQLite reopen.
#![cfg(feature = "sqlite")]
use nanocodex_durability::{Admission, BeginStep, DurableSession, ReplaySafety, SqliteStore};
use std::{fs::OpenOptions, io::Write, path::Path, process::Command};

fn policy(value: &str) -> ReplaySafety {
    match value {
        "safe" => ReplaySafety::Safe,
        "unsafe" => ReplaySafety::Unsafe,
        _ => panic!("bad policy"),
    }
}

#[tokio::test]
async fn replay_worker() -> eyre::Result<()> {
    let Ok(dir) = std::env::var("NANOCODEX_REPLAY_TEST_DIR") else {
        return Ok(());
    };
    let phase = std::env::var("NANOCODEX_REPLAY_TEST_PHASE")?;
    let safety = policy(&std::env::var("NANOCODEX_REPLAY_TEST_SAFETY")?);
    let session = DurableSession::open(
        SqliteStore::open(Path::new(&dir).join("state.sqlite"))?,
        "session",
    )
    .await?;
    let admission = session.admit("operation", &"one exact request").await?;
    assert!(matches!(
        admission,
        Admission::Accepted | Admission::Pending
    ));
    session.begin_attempt("operation").await?;
    let admission = session
        .begin_step_with_replay(
            "operation",
            "effect",
            "external",
            &"same idempotency identity",
            safety,
        )
        .await?;
    let outcome = match admission {
        BeginStep::Execute => {
            // A real external effect survives termination without Rust destructors.
            let mut count = OpenOptions::new()
                .create(true)
                .append(true)
                .open(Path::new(&dir).join("invocations"))?;
            writeln!(count, "invoke")?;
            count.sync_all()?;
            // Replacing one keyed value is the opted-in idempotent effect.
            std::fs::write(Path::new(&dir).join("keyed-value"), "one value")?;
            if phase == "crash-pending" {
                std::process::exit(73);
            }
            session
                .complete_step("operation", "effect", &"exact receipt")
                .await?;
            if phase == "crash-completed" {
                std::process::exit(73);
            }
            "executed"
        }
        BeginStep::Replay(receipt) => {
            assert_eq!(receipt.decode::<String>()?, "exact receipt");
            "replayed"
        }
        BeginStep::OutcomeUnknown => {
            session
                .complete_step("operation", "effect", &"outcome unknown")
                .await?;
            "unknown"
        }
    };
    std::fs::write(Path::new(&dir).join("outcome"), outcome)?;
    println!("{phase}: {outcome}");
    Ok(())
}

fn worker(dir: &Path, phase: &str, safety: &str, code: i32) -> eyre::Result<String> {
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", "replay_worker", "--nocapture"])
        .env("NANOCODEX_REPLAY_TEST_DIR", dir)
        .env("NANOCODEX_REPLAY_TEST_PHASE", phase)
        .env("NANOCODEX_REPLAY_TEST_SAFETY", safety)
        .output()?;
    let trace = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(code), "{trace}");
    Ok(trace)
}

#[test]
fn process_crash_recovery_never_repeats_unclassified_effects() -> eyre::Result<()> {
    for (initial, current, terminal, expected, invocations) in [
        ("unsafe", "unsafe", false, "unknown", 1),
        ("unsafe", "safe", false, "unknown", 1),
        ("safe", "unsafe", false, "unknown", 1),
        ("safe", "safe", false, "executed", 2),
        ("unsafe", "unsafe", true, "replayed", 1),
        ("safe", "unsafe", true, "replayed", 1),
    ] {
        let dir = tempfile::tempdir()?;
        let trace = worker(
            dir.path(),
            if terminal {
                "crash-completed"
            } else {
                "crash-pending"
            },
            initial,
            73,
        )?;
        let resumed = worker(dir.path(), "reopen", current, 0)?;
        assert_eq!(
            std::fs::read_to_string(dir.path().join("outcome"))?,
            expected
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("invocations"))?
                .lines()
                .count(),
            invocations
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("keyed-value"))?,
            "one value"
        );
        println!(
            "persisted={initial} current={current} completed={terminal} result={expected} invocations={invocations}\n{trace}{resumed}"
        );
    }
    Ok(())
}
