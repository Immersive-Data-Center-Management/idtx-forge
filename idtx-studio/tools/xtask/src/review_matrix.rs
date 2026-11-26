//! Executable review gates. Each lane writes a revision-bound result manifest.
use std::{fs, path::{Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};
use anyhow::{Context, Result, bail};
use clap::Args;
use sha2::{Digest, Sha256};

/// Parsed `verify-review` options.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct VerifyReviewOptions {
    /// Evidence lane to run: core | usd | cross | windows | server.
    #[arg(long, default_value = "core", value_name = "LANE")]
    pub lane: String,
    /// Output directory for the per-lane evidence (logs + report.json).
    #[arg(long, default_value = "target/review-matrix", value_name = "DIR")]
    pub out: PathBuf,
    /// List the jobs the lane would run, then exit without running them.
    #[arg(long)]
    pub list: bool,
}

#[derive(Debug)]
struct Job { name: &'static str, args: Vec<String> }
fn job(name: &'static str, args: &[&str]) -> Job { Job { name, args: args.iter().map(|s| (*s).into()).collect() } }

fn jobs(lane: &str) -> Result<Vec<Job>> {
    Ok(match lane {
        "core" => vec![
            job("tooling", &["test", "--locked", "-p", "xtask"]),
            job("auth-collab-policy", &["test", "--locked", "-p", "idtx-auth", "-p", "idtx-collab", "-p", "idtx-net-guard", "--", "--test-threads=1"]),
            job("behavior-native", &["test", "--locked", "-p", "idtx-behavior", "--features", "net", "--", "--test-threads=1"]),
            job("behavior-pulley", &["test", "--locked", "-p", "idtx-behavior", "--features", "net,visionos", "--", "--test-threads=1"]),
            job("sdk", &["test", "--locked", "-p", "idtx-behavior-sdk"]),
            job("behavior-build-cache", &["test", "--locked", "-p", "idtx-behavior-build", "--test", "cache_contract"]),
            job("login-native", &["test", "--locked", "-p", "idtx-behavior", "--features", "net", "--test", "login_slice", "--", "--ignored", "--test-threads=1"]),
            job("login-pulley", &["test", "--locked", "-p", "idtx-behavior", "--features", "net,visionos", "--test", "login_slice", "--", "--ignored", "--test-threads=1"]),
        ],
        "usd" => vec![
            job("workspace", &["check", "--locked", "--workspace"]),
            job("project-authoring", &["test", "--locked", "-p", "idtx-project", "-p", "idtx-authoring", "--", "--test-threads=1"]),
            job("shell-authoring", &["test", "--locked", "-p", "idtx-studio-shell", "--lib", "--no-default-features", "--features", "authoring"]),
            job("shell-cooking", &["test", "--locked", "-p", "idtx-studio-shell", "--lib", "--features", "cooking,remote,metrics"]),
            job("usd", &["test", "--locked", "-p", "idtx-usd", "--features", "authoring", "--", "--test-threads=1"]),
            job("resolver", &["test", "--locked", "-p", "idtx-usd-resolver-http", "--", "--test-threads=1"]),
            job("render-budgets", &["test", "--locked", "-p", "idtx-render", "--lib", "--test", "texture_budget", "--features", "metrics"]),
            job("render-udim", &["test", "--locked", "-p", "idtx-render", "--test", "render_integration", "udim", "--", "--ignored", "--test-threads=1"]),
            job("session", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics", "--", "--test-threads=1"]),
            job("session-invalidation-native", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics", "invalidation", "--", "--include-ignored", "--test-threads=1"]),
            job("session-invalidation-pulley", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics,idtx-behavior/visionos", "invalidation", "--", "--include-ignored", "--test-threads=1"]),
            job("session-prototypes-native", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics", "runtime_prototypes", "--", "--include-ignored", "--test-threads=1"]),
            job("session-prototypes-pulley", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics,idtx-behavior/visionos", "runtime_prototypes", "--", "--include-ignored", "--test-threads=1"]),
            job("session-animation-native", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics", "animation_playback", "--", "--include-ignored", "--test-threads=1"]),
            job("session-animation-pulley", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics,idtx-behavior/visionos", "animation_playback", "--", "--include-ignored", "--test-threads=1"]),
            job("session-bindings-native", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics", "behavior_bindings", "--", "--include-ignored", "--test-threads=1"]),
            job("session-bindings-pulley", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics,idtx-behavior/visionos", "behavior_bindings", "--", "--include-ignored", "--test-threads=1"]),
            job("widget-precision", &["test", "--locked", "-p", "idtx-scene", "--test", "widget_precision", "--", "--test-threads=1"]),
            job("session-exec-refresh", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,remote,openexec,physics,visionos,metrics", "exec::session_tests", "--", "--include-ignored", "--test-threads=1"]),
            job("session-physics-native", &["test", "--locked", "-p", "idtx-studio-session", "--test", "physics_phase1", "--features", "authoring,remote,openexec,physics,visionos,metrics", "--", "--include-ignored", "--test-threads=1"]),
            job("session-physics-pulley", &["test", "--locked", "-p", "idtx-studio-session", "--test", "physics_phase1", "--features", "authoring,remote,openexec,physics,visionos,metrics,idtx-behavior/visionos", "--", "--include-ignored", "--test-threads=1"]),
            job("collaboration-usd", &["test", "--locked", "-p", "idtx-studio-session", "--test", "collaboration", "--features", "authoring,remote,metrics", "--", "--ignored", "--test-threads=1"]),
            job("widgets", &["test", "--locked", "-p", "idtx-studio-session", "--test", "behavior_widget", "--features", "authoring", "--", "--ignored", "--test-threads=1"]),
            job("session-final-metrics", &["test", "--locked", "-p", "idtx-studio-session", "--lib", "--features", "authoring,visionos,metrics", "every_session_flushes", "--", "--ignored", "--test-threads=1"]),
            job("texture-policy", &["test", "--locked", "-p", "idtx-session-orchestration", "--test", "texture_policy", "--", "--ignored", "--test-threads=1"]),
            job("runtime-transitions", &["test", "--locked", "-p", "idtx-runtime-core", "--features", "authoring,remote,openexec,physics,mock-xr,metrics", "--test", "level_transition_driver", "--", "--ignored", "--test-threads=1"]),
            job("usd-locking", &["test", "--locked", "-p", "idtx-usd", "--features", "authoring,test-support", "--test", "background_open_concurrency", "--", "--ignored", "--test-threads=1"]),
            job("transition-budget", &["test", "--locked", "-p", "idtx-session-orchestration", "--test", "transition_budget", "--", "--ignored", "--test-threads=1"]),
            job("cook-package", &["test", "--locked", "-p", "idtx-build", "--", "--test-threads=1"]),
            job("native-runtime", &["build", "--locked", "-p", "idtx-runtime", "--no-default-features", "--features", "metrics"]),
            job("package-launch", &["test", "--locked", "-p", "idtx-build", "--test", "package_launch", "--", "--ignored", "--test-threads=1"]),
        ],
        "cross" => vec![
            job("visionos-metrics", &["xtask", "build-visionos-runtime", "--with-remote", "--with-openexec", "--with-metrics"]),
            job("visionos", &["xtask", "build-visionos-runtime", "--with-remote", "--with-openexec"]),
            job("quest-metrics", &["xtask", "build-quest-runtime", "--with-remote", "--with-openexec", "--with-metrics"]),
            job("quest", &["xtask", "build-quest-runtime", "--with-remote", "--with-openexec"]),
            job("swift-xros", &["test", "--locked", "-p", "idtx-build", "--lib", "generated_swift_shell_typechecks_for_visionos", "--", "--ignored", "--test-threads=1"]),
        ],
        "windows" => vec![
            job("desktop", &["build", "--locked", "-p", "idtx-runtime", "--no-default-features"]),
            job("pcvr", &["build", "--locked", "-p", "idtx-runtime", "--features", "pcvr,remote,openexec"]),
            job("pcvr-metrics", &["build", "--locked", "-p", "idtx-runtime", "--features", "pcvr,remote,openexec,metrics"]),
            job("windows-publication", &["test", "--locked", "-p", "idtx-build", "--lib", "publication::tests", "--", "--test-threads=1"]),
        ],
        "server" => vec![job("deployed-server", &["test", "--locked", "-p", "idtx-collab", "--test", "server_compatibility", "--", "--ignored", "--test-threads=1"])],
        _ => bail!("unknown lane {lane}; choose core, usd, cross, windows, or server"),
    })
}

fn digest(path: &Path) -> Result<String> {
    use std::{io::Read, fmt::Write as _};
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new(); let mut buf = vec![0u8; 64 * 1024];
    loop { let n = file.read(&mut buf)?; if n == 0 { break; } hash.update(&buf[..n]); }
    Ok(hash.finalize().iter().fold(String::new(), |mut text, b| { let _ = write!(text, "{b:02x}"); text }))
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git").current_dir(root).args(args).output()
        .with_context(|| format!("git {} in {}", args.join(" "), root.display()))?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr));
    }
    Ok(output.stdout)
}

/// Bind evidence to the whole repository (Forge and CI included), not only the
/// Studio workspace. Untracked source participates without staging user files.
/// Ignore our output tree even when --out points to a non-ignored directory.
fn source_snapshot(root: &Path, out: &Path) -> Result<(Vec<u8>, serde_json::Value)> {
    let diff = git(root, &["diff", "--binary", "HEAD", "--", "."])?;
    let paths = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut entries = Vec::new();
    for path in paths.split(|b| *b == 0).filter(|path| !path.is_empty()) {
        let name = std::str::from_utf8(path).context("untracked source path is not UTF-8")?;
        let path = root.join(name);
        if path.starts_with(out) { continue; }
        let metadata = fs::symlink_metadata(&path).with_context(|| format!("fingerprint {}", path.display()))?;
        let (kind, hash) = if metadata.file_type().is_symlink() {
            // Hash the link identity, never read an arbitrary external target.
            let target = fs::read_link(&path)?;
            let target = target.to_str().context("untracked symlink target is not UTF-8")?;
            ("symlink", Sha256::digest(target.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect())
        } else if metadata.is_file() {
            ("file", digest(&path)?)
        } else {
            bail!("unsupported untracked source type: {}", path.display());
        };
        entries.push(serde_json::json!({ "path": name, "kind": kind, "sha256": hash }));
    }
    entries.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok((diff, serde_json::Value::Array(entries)))
}

fn passed_tests(output: &str) -> usize {
    output.lines().filter_map(|line| line.strip_prefix("test result: ok. "))
        .filter_map(|line| line.split_whitespace().next()?.parse::<usize>().ok()).sum()
}

fn usd_install(variable: &str) -> Result<serde_json::Value> {
    let root = PathBuf::from(std::env::var_os(variable).with_context(|| format!("set {variable} to OpenUSD 26.11.1"))?).canonicalize()?;
    if !root.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("openusd-26.11.1")) {
        bail!("{variable} must identify the approved openusd-26.11.1 installation");
    }
    let header = fs::read_to_string(root.join("include/pxr/pxr.h"))?;
    if !header.lines().any(|line| line.split_whitespace().collect::<Vec<_>>() == ["#define", "PXR_VERSION", "2611"]) { bail!("{variable}: expected PXR_VERSION 2611"); }
    let monolith = ["lib/libusd_ms.dylib", "lib/libusd_ms.so", "lib/usd_ms.dll", "bin/usd_ms.dll"].iter()
        .map(|p| root.join(p)).find(|p| p.is_file()).context("OpenUSD monolith missing")?;
    Ok(serde_json::json!({ "variable": variable, "prefix": root, "version": "26.11.1", "monolith_sha256": digest(&monolith)? }))
}

pub fn run(opts: VerifyReviewOptions) -> Result<()> {
    let VerifyReviewOptions { lane, out, list } = opts;
    let jobs = jobs(&lane)?;
    if list { for job in jobs { println!("{}: cargo {}", job.name, job.args.join(" ")); } return Ok(()); }
    if lane == "cross" && !cfg!(target_os = "macos") { bail!("cross lane requires macOS/xros plus Android SDK"); }
    if lane == "windows" && !cfg!(windows) { bail!("windows lane requires a Windows host; host cfg checks do not validate PCVR"); }
    if lane == "server" {
        for key in ["IDTX_TEST_SERVER_URL", "IDTX_TEST_SERVER_TOKEN", "IDTX_TEST_SERVER_STAGE", "IDTX_TEST_SERVER_REFERENCE"] {
            if std::env::var(key).unwrap_or_default().is_empty() { bail!("server lane requires {key} for a disposable test stage"); }
        }
    }
    let root = crate::workspace_root().context("locate verify-review workspace")?.canonicalize()?;
    let lock = root.join("Cargo.lock");
    if !lock.is_file() { bail!("verify-review workspace {} has no Cargo.lock", root.display()); }
    run_jobs(lane, out, jobs, root)
}

fn run_jobs(lane: String, out: PathBuf, jobs: Vec<Job>, root: PathBuf) -> Result<()> {
    let out = if out.is_absolute() { out } else { root.join(out) }.join(&lane);
    fs::create_dir_all(&out)?;
    let out = out.canonicalize()?;
    let mut installs = vec![];
    if lane != "core" && lane != "server" { installs.push(usd_install("IDTX_USD_DIR")?); }
    if lane == "cross" { installs.push(usd_install("IDTX_USD_DIR_ANDROID")?); installs.push(usd_install("IDTX_USD_DIR_VISIONOS")?); }
    let capture = |program: &str, args: &[&str]| Command::new(program).current_dir(&root).args(args).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
    let repository = PathBuf::from(String::from_utf8(git(&root, &["rev-parse", "--show-toplevel"])?)?.trim()).canonicalize()?;
    let (diff, untracked) = source_snapshot(&repository, &out)?;
    fs::write(out.join("source.patch"), &diff)?;
    let untracked_bytes = serde_json::to_vec(&untracked)?;
    fs::write(out.join("untracked.json"), &untracked_bytes)?;
    let mut source_hash = Sha256::new();
    source_hash.update((diff.len() as u64).to_le_bytes());
    source_hash.update(&diff);
    source_hash.update(&untracked_bytes);
    let mut report = serde_json::json!({ "schema_version": 2, "lane": lane,
        "workspace": root, "repository": repository,
        "revision": capture("git", &["rev-parse", "HEAD"]), "os": std::env::consts::OS,
        "source_diff_sha256": digest(&out.join("source.patch"))?,
        "untracked_source": untracked, "source_sha256": source_hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "rustc": capture("rustc", &["-Vv"]), "nightly": capture("rustup", &["run", "nightly", "rustc", "-Vv"]),
        "cargo_lock_sha256": digest(&root.join("Cargo.lock"))?, "usd": installs, "jobs": [],
        "evidence_boundary": "Compile/test lanes do not establish headset launch, pacing, or deployed-server compatibility; those require their explicit environment gates." });
    let report_path = out.join("report.json");
    for job in jobs {
        let log = out.join(format!("{}.log", job.name));
        println!("verify-review: {} ({})", job.name, log.display());
        let file = fs::File::create(&log)?;
        let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        command.current_dir(&root).args(&job.args).stdout(Stdio::from(file.try_clone()?)).stderr(Stdio::from(file));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        if job.name == "package-launch" { command.env("IDTX_TEST_RUNTIME", root.join("target/debug").join(if cfg!(windows) { "idtx-runtime.exe" } else { "idtx-runtime" })); }
        let started = Instant::now();
        let mut child = command.spawn().with_context(|| format!("spawn {}", job.name))?;
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait()? { break status; }
            if started.elapsed() > Duration::from_secs(3600) {
                timed_out = true;
                #[cfg(unix)]
                let _ = Command::new("/bin/kill").args(["-KILL", "--", &format!("-{}", child.id())]).status();
                #[cfg(windows)]
                let _ = Command::new("taskkill").args(["/F", "/T", "/PID", &child.id().to_string()]).status();
                let _ = child.kill(); break child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let text = fs::read_to_string(&log).unwrap_or_default();
        let future_error = text.contains("recursion_depth_exceeding_limit");
        let executed_tests = passed_tests(&text);
        let empty_test_job = job.args.first().is_some_and(|arg| arg == "test") && executed_tests == 0;
        let passed = status.success() && !future_error && !timed_out && !empty_test_job;
        report["jobs"].as_array_mut().unwrap().push(serde_json::json!({ "name": job.name, "args": job.args,
            "passed": passed, "executed_tests": executed_tests, "timed_out": timed_out, "exit_code": status.code(), "elapsed_ms": started.elapsed().as_millis(), "log": log }));
        fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
        if !passed { bail!("{} failed; see {}", job.name, report_path.display()); }
    }
    println!("verify-review: passed; {}", report_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_smoke_uses_workspace_lockfile_and_writes_evidence() {
        let root = crate::workspace_root().unwrap().canonicalize().unwrap();
        assert_eq!(root, Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().canonicalize().unwrap());
        let out = tempfile::tempdir().unwrap();
        run_jobs("core".into(), out.path().to_owned(),
            vec![job("smoke", &["metadata", "--locked", "--no-deps", "--format-version", "1"])], root.clone()).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&fs::read(out.path().join("core/report.json")).unwrap()).unwrap();
        assert_eq!(report["workspace"], root.to_str().unwrap());
        assert_eq!(report["cargo_lock_sha256"], digest(&root.join("Cargo.lock")).unwrap());
        assert_eq!(report["jobs"][0]["passed"], true);
        let metadata: serde_json::Value = serde_json::from_slice(&fs::read(out.path().join("core/smoke.log")).unwrap()).unwrap();
        assert_eq!(metadata["workspace_root"], root.to_str().unwrap());
        assert!(out.path().join("core/source.patch").is_file());
        assert!(out.path().join("core/untracked.json").is_file());
    }

    #[test]
    fn fingerprint_includes_forge_ci_and_untracked_bytes_but_not_its_output() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "--quiet"]).unwrap();
        fs::create_dir(root.join("idtx-forge")).unwrap();
        fs::create_dir(root.join(".github")).unwrap();
        fs::write(root.join("idtx-forge/a.cpp"), "original").unwrap();
        fs::write(root.join(".github/ci.yaml"), "original").unwrap();
        git(root, &["add", "."]).unwrap();
        git(root, &["-c", "user.name=Test", "-c", "user.email=test@example.invalid",
            "-c", "commit.gpgsign=false", "-c", "core.hooksPath=.git/no-test-hooks",
            "commit", "--quiet", "-m", "fixture"]).unwrap();
        fs::write(root.join("idtx-forge/a.cpp"), "changed").unwrap();
        fs::write(root.join(".github/ci.yaml"), "changed").unwrap();
        fs::write(root.join("new.rs"), "first").unwrap();
        let out = root.join("evidence");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("log.txt"), "not source").unwrap();
        let (diff, first) = source_snapshot(root, &out).unwrap();
        let diff = String::from_utf8(diff).unwrap();
        assert!(diff.contains("idtx-forge/a.cpp"));
        assert!(diff.contains(".github/ci.yaml"));
        assert_eq!(first.as_array().unwrap().len(), 1);
        assert_eq!(first[0]["path"], "new.rs");
        fs::write(root.join("new.rs"), "second").unwrap();
        let (_, second) = source_snapshot(root, &out).unwrap();
        assert_ne!(first[0]["sha256"], second[0]["sha256"]);
        assert!(source_snapshot(&root.join("missing"), &out).is_err());
    }
    #[test]
    fn empty_or_ignored_only_test_binaries_do_not_count_as_execution() {
        assert_eq!(passed_tests("test result: ok. 0 passed; 0 failed; 7 ignored"), 0);
        assert_eq!(passed_tests("test result: ok. 3 passed; 0 failed\ntest result: ok. 0 passed; 0 failed"), 3);
    }
    #[test]
    fn required_lanes_cover_native_pulley_target_metrics_and_real_launch() {
        let cross = jobs("cross").unwrap();
        for target in ["quest", "visionos"] {
            assert!(cross.iter().any(|j| j.name == target && !j.args.iter().any(|a| a == "--with-metrics")));
            assert!(cross.iter().any(|j| j.name == format!("{target}-metrics") && j.args.iter().any(|a| a == "--with-metrics")));
        }
        assert!(jobs("usd").unwrap().iter().any(|j| j.name == "package-launch"));
        assert!(jobs("windows").unwrap().iter().any(|j| j.name == "windows-publication"));
        assert!(jobs("core").unwrap().iter().any(|j| j.name == "login-pulley"));
        assert!(jobs("typo").is_err());
        let usd = jobs("usd").unwrap();
        for name in ["session-invalidation-native", "session-invalidation-pulley",
            "session-prototypes-native", "session-prototypes-pulley", "session-exec-refresh",
            "session-animation-native", "session-animation-pulley", "session-bindings-native", "session-bindings-pulley",
            "session-physics-native", "session-physics-pulley"] {
            let job = usd.iter().find(|job| job.name == name).unwrap();
            assert!(job.args.iter().any(|arg| arg == "--include-ignored"));
            let features = &job.args[job.args.iter().position(|arg| arg == "--features").unwrap() + 1];
            assert_eq!(features.contains("idtx-behavior/visionos"), name.ends_with("pulley"));
        }
    }
}
