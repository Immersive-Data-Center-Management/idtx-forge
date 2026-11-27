use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run potentially nonterminating guest code in a killable, reaped child. Guest
/// execution timing is measured by the caller after compilation, independently
/// of this outer process/compilation deadline.
pub fn with_watchdog(test: &str, run: impl FnOnce()) {
    const CHILD: &str = "IDTX_BEHAVIOR_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        run();
        return;
    }
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, test)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn regression child");
    let deadline = Instant::now() + Duration::from_secs(5);
    let timed_out = loop {
        if child.try_wait().expect("wait for child").is_some() { break false; }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break true;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().expect("reap regression child");
    assert!(!timed_out && output.status.success(),
        "{test}: watchdog_expired={timed_out}, status={}\n{}\n{}",
        output.status, String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
