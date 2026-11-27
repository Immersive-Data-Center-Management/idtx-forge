//! Real `.apk` assembly for the Quest standalone target.
//!
//! [`AndroidApkPackager`](crate::AndroidApkPackager) stages a gradle-shaped tree
//! (`AndroidManifest.xml` + `jniLibs/arm64-v8a/*.so` + `assets/`); this module
//! turns that tree into a **signed, installable `.apk`** without gradle, driving
//! the Android SDK build-tools directly:
//!
//! ```text
//! aapt2 link   manifest + assets        -> unaligned.apk (binary xml + resources.arsc)
//! jar uf       lib/arm64-v8a/*.so       -> insert native libs under lib/
//! zipalign -f 4                         -> aligned.apk
//! keytool                               -> debug.jks           (only for a debug identity)
//! apksigner sign                        -> <out>.apk           (v2/v3 signature)
//! apksigner verify --print-certs        -> fail-closed check
//! ```
//!
//! The pipeline is proven end-to-end against the installed Android SDK (produces
//! a launchable `NativeActivity` APK with `native-code 'arm64-v8a'` and the Quest
//! VR feature). It fails closed: any missing tool or non-zero tool exit aborts
//! with a precise [`ApkError`]; a caller that cannot find the SDK is expected to
//! leave the staging tree in place rather than fabricate a broken bundle.
//!
//! Only pure argument-vector builders and the SDK version-selection logic are
//! unit-tested here; the end-to-end assembly runs behind an SDK-presence gate so
//! it exercises the real tools on a host that has them and skips cleanly where it
//! does not (CLAUDE.md's device/tool-bound discipline).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Default Android `minSdkVersion` for the Quest APK (matches the manifest
/// template's `<uses-sdk>` and the Quest OS floor).
pub const ANDROID_MIN_SDK: u32 = 30;
/// Default Android `targetSdkVersion` for the Quest APK (matches the manifest).
pub const ANDROID_TARGET_SDK: u32 = 32;

/// The APK on-device native-lib directory (`lib/<abi>/`). The staging tree uses
/// the gradle `jniLibs/<abi>/` convention; the assembler remaps to this.
const APK_LIB_DIR: &str = "lib";
/// The single ABI Quest ships (64-bit ARM). Mirrors
/// [`ANDROID_ABI_DIR`](crate::ANDROID_ABI_DIR).
const APK_ABI: &str = "arm64-v8a";

/// A failure assembling the `.apk`.
#[derive(Debug, thiserror::Error)]
pub enum ApkError {
    /// No Android SDK root could be located at all (neither configured, nor in
    /// `ANDROID_HOME`/`ANDROID_SDK_ROOT`, nor the per-OS default).
    ///
    /// Distinct from [`ApkError::ToolNotFound`] so callers can treat "no SDK on
    /// this host" (the expected cook-only / defer-to-CI case) differently from
    /// "SDK present but incomplete/misconfigured" (a real build failure).
    #[error("no Android SDK found ({hint})")]
    SdkRootNotFound {
        /// How to make the SDK discoverable.
        hint: String,
    },
    /// An explicit `[build.android]` override (`sdk` / `jdk`) names a path that
    /// is not an existing directory.
    ///
    /// Distinct from [`ApkError::SdkRootNotFound`]: a mis-typed *configured*
    /// path is a real user error that must abort the build, whereas a genuinely
    /// absent SDK (no override, nothing in the environment) is the benign
    /// defer-to-CI case. Keeping them separate lets the caller swallow the
    /// latter while surfacing the former.
    #[error("configured Android {what} path does not exist: {path}")]
    ConfiguredPathNotFound {
        /// Which override was wrong (`"sdk"` or `"jdk"`).
        what: &'static str,
        /// The configured path that does not resolve to a directory.
        path: PathBuf,
    },
    /// A required Android SDK / JDK tool was not found on this host.
    #[error("android tool not found: {tool} ({hint})")]
    ToolNotFound {
        /// The tool that could not be located (`aapt2`, `apksigner`, `jar`, …).
        tool: String,
        /// How to make it discoverable (env var / install hint).
        hint: String,
    },
    /// A tool ran but exited non-zero.
    #[error("{tool} failed (exit {status:?}): {stderr}")]
    CommandFailed {
        /// The tool that failed.
        tool: String,
        /// The process exit code (`None` if killed by a signal).
        status: Option<i32>,
        /// The captured stderr (trimmed).
        stderr: String,
    },
    /// A tool could not be spawned at all.
    #[error("failed to spawn {tool}: {source}")]
    Spawn {
        /// The tool that could not be launched.
        tool: String,
        /// The underlying spawn error.
        source: std::io::Error,
    },
    /// A filesystem operation around assembly failed.
    #[error("apk assembly I/O at {path}: {source}")]
    Io {
        /// The path the operation was on.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The staging tree is missing a required input.
    #[error("apk staging is missing {what}: {path}")]
    MissingInput {
        /// What was expected (manifest / native lib / assets).
        what: String,
        /// Where it was looked for.
        path: PathBuf,
    },
}

/// A resolved Android SDK + JDK tool set, located once and reused per assembly.
///
/// Paths are concrete executables (with the host's extension already resolved,
/// e.g. `aapt2.exe` / `apksigner.bat` on Windows) so assembly never re-probes.
#[derive(Debug, Clone)]
pub struct AndroidSdk {
    /// `aapt2` — compiles the binary manifest + `resources.arsc`, bundles assets.
    pub aapt2: PathBuf,
    /// `zipalign` — 4-byte-aligns the archive before signing.
    pub zipalign: PathBuf,
    /// `apksigner` — v2/v3 signs and verifies the APK.
    pub apksigner: PathBuf,
    /// `android.jar` for the chosen platform — aapt2's `-I` include.
    pub android_jar: PathBuf,
    /// JDK `jar` — inserts the native-lib entries into the APK zip.
    pub jar: PathBuf,
    /// JDK `keytool` — generates a debug keystore when no identity is supplied.
    pub keytool: PathBuf,
}

impl AndroidSdk {
    /// Directory holding the resolved JDK executables (`jar`'s parent), or
    /// `None` when `jar` has no parent (unreachable for a discovered tool).
    ///
    /// `apksigner` is a wrapper script that re-resolves `java` from `PATH`
    /// itself — it ignores the absolute paths we hand `jar`/`keytool`. On a host
    /// whose `PATH` `java` is a launcher stub (the macOS `/usr/bin/java` that
    /// prints "Unable to locate a Java Runtime", or a JRE-less box), that lookup
    /// hits the stub and the sign/verify step fails even though a real JDK is
    /// configured via `[build.android].jdk`. Prepending this directory to the
    /// child `PATH` (see [`apksigner_env`]) puts the resolved JDK's `java` first
    /// so the script finds the same JDK we validated.
    fn jdk_bin(&self) -> Option<&Path> {
        self.jar.parent()
    }

    /// `(PATH, JAVA_HOME)` overrides for an `apksigner` child so its internal
    /// `java` lookup finds the resolved JDK. `PATH` is this process's `PATH`
    /// with the JDK `bin` prepended (so the real `java` shadows any launcher
    /// stub); `JAVA_HOME` points at the JDK root for tools that honour it.
    /// Returns `(None, None)` when the JDK bin can't be derived — the child
    /// then just inherits the ambient environment.
    fn apksigner_env(&self) -> (Option<String>, Option<String>) {
        let Some(bin) = self.jdk_bin() else {
            return (None, None);
        };
        let sep = if cfg!(windows) { ";" } else { ":" };
        let path = match std::env::var_os("PATH") {
            Some(existing) => format!("{}{sep}{}", bin.display(), existing.to_string_lossy()),
            None => bin.display().to_string(),
        };
        let java_home = bin.parent().map(|home| home.display().to_string());
        (Some(path), java_home)
    }

    /// Discover the SDK + JDK tools from the environment.
    ///
    /// Equivalent to [`discover_with`](Self::discover_with)`(None, None)`: SDK
    /// root resolves from `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then the
    /// per-OS default; the JDK from `JAVA_HOME` / `java` on `PATH`.
    ///
    /// # Errors
    ///
    /// [`ApkError::SdkRootNotFound`] when no SDK root exists at all;
    /// [`ApkError::ToolNotFound`] when a root exists but a required piece is
    /// missing.
    pub fn discover() -> Result<Self, ApkError> {
        Self::discover_with(None, None)
    }

    /// Discover the SDK + JDK tools, preferring explicit `sdk`/`jdk` roots
    /// (typically from the project's `[build.android]` config) over the
    /// environment.
    ///
    /// `sdk` overrides the Android SDK root (`ANDROID_HOME`/`ANDROID_SDK_ROOT`/
    /// per-OS default); `jdk` overrides the JDK root (`JAVA_HOME`/`java` on
    /// `PATH`). An override that is not an existing directory is rejected (not
    /// silently ignored) so a mis-typed config surfaces instead of falling back
    /// to a surprise toolchain. Within the SDK root the **highest-versioned**
    /// `build-tools/` and `platforms/android-NN/` are chosen. `jar` and
    /// `keytool` are resolved independently (JDK bin dir, then directly on
    /// `PATH`) so a launcher-only `java` shim (the Oracle `javapath` layout on
    /// Windows) does not block a full JDK that is otherwise reachable.
    ///
    /// # Errors
    ///
    /// [`ApkError::ConfiguredPathNotFound`] when an explicit `sdk`/`jdk` override
    /// is not an existing directory; [`ApkError::SdkRootNotFound`] when no SDK
    /// root exists at all (no override, nothing in the environment);
    /// [`ApkError::ToolNotFound`] naming the first missing piece otherwise.
    pub fn discover_with(sdk: Option<&Path>, jdk: Option<&Path>) -> Result<Self, ApkError> {
        // An explicit override that does not resolve to a directory is a real
        // misconfiguration (a typo in `[build.android]`), not the benign "no SDK
        // on this host" case. Reject it up-front with a distinct error so the
        // build aborts and names the bad path, rather than silently falling back
        // to a surprise toolchain (the env SDK, or `JAVA_HOME`/`PATH` for the
        // JDK). `SdkRootNotFound` stays reserved for a genuinely absent SDK.
        if let Some(root) = sdk
            && !root.is_dir() {
                return Err(ApkError::ConfiguredPathNotFound {
                    what: "sdk",
                    path: root.to_path_buf(),
                });
            }
        if let Some(root) = jdk
            && !root.is_dir() {
                return Err(ApkError::ConfiguredPathNotFound {
                    what: "jdk",
                    path: root.to_path_buf(),
                });
            }

        let sdk = resolve_sdk_root(sdk).ok_or_else(|| ApkError::SdkRootNotFound {
            hint: "set [build.android].sdk in project.toml or ANDROID_HOME".to_owned(),
        })?;

        let build_tools_root = sdk.join("build-tools");
        let bt_ver = highest_subdir(&build_tools_root, parse_dotted_version).ok_or_else(|| {
            ApkError::ToolNotFound {
                tool: "build-tools".to_owned(),
                hint: format!(
                    "install Android build-tools under {}",
                    build_tools_root.display()
                ),
            }
        })?;
        let bt = build_tools_root.join(&bt_ver);

        let aapt2 = tool_in(&bt, "aapt2").ok_or_else(|| missing("aapt2", &bt))?;
        let zipalign = tool_in(&bt, "zipalign").ok_or_else(|| missing("zipalign", &bt))?;
        let apksigner = tool_in(&bt, "apksigner").ok_or_else(|| missing("apksigner", &bt))?;

        let platforms_root = sdk.join("platforms");
        let plat = highest_subdir(&platforms_root, parse_platform_version).ok_or_else(|| {
            ApkError::ToolNotFound {
                tool: "platform".to_owned(),
                hint: format!(
                    "install an Android platform under {}",
                    platforms_root.display()
                ),
            }
        })?;
        let android_jar = platforms_root.join(&plat).join("android.jar");
        if !android_jar.is_file() {
            return Err(missing("android.jar", &platforms_root.join(&plat)));
        }

        // Resolve `jar`/`keytool` independently rather than insisting both sit
        // beside the resolved `java` launcher: on Windows the Oracle installer
        // drops a launcher-only shim (java.exe without jar/keytool) into
        // `…\Common Files\Oracle\Java\javapath\`, and `JAVA_HOME` may point at a
        // JRE. `jdk_tool` tries the JDK bin dir(s) and then `PATH` directly.
        let jar = jdk_tool("jar", jdk).ok_or_else(|| ApkError::ToolNotFound {
            tool: "jar".to_owned(),
            hint: "set [build.android].jdk in project.toml or JAVA_HOME (JDK 17+)".to_owned(),
        })?;
        let keytool = jdk_tool("keytool", jdk).ok_or_else(|| ApkError::ToolNotFound {
            tool: "keytool".to_owned(),
            hint: "set [build.android].jdk in project.toml or JAVA_HOME (JDK 17+)".to_owned(),
        })?;

        Ok(Self {
            aapt2,
            zipalign,
            apksigner,
            android_jar,
            jar,
            keytool,
        })
    }
}

fn missing(tool: &str, dir: &Path) -> ApkError {
    ApkError::ToolNotFound {
        tool: tool.to_owned(),
        hint: format!("expected under {}", dir.display()),
    }
}

/// How the produced APK is signed.
#[derive(Debug, Clone, Default)]
pub enum ApkSigning {
    /// Generate a throwaway debug keystore (`androiddebugkey`, pass `android`)
    /// and sign with it — installable for local on-device testing, not for
    /// store distribution.
    #[default]
    Debug,
    /// Sign with a caller-provided keystore.
    Keystore {
        /// Path to the `.jks`/`.keystore` file.
        path: PathBuf,
        /// Keystore password.
        store_pass: String,
        /// Signing key alias within the keystore.
        key_alias: String,
        /// Key password (often equal to the store password).
        key_pass: String,
    },
}

/// Assembles a signed `.apk` from a staging tree produced by
/// [`AndroidApkPackager`](crate::AndroidApkPackager).
#[derive(Debug, Clone)]
pub struct ApkAssembler {
    sdk: AndroidSdk,
    staging: PathBuf,
    output: PathBuf,
    min_sdk: u32,
    target_sdk: u32,
    signing: ApkSigning,
}

impl ApkAssembler {
    /// A new assembler over `staging` (the packager's output dir) writing the
    /// final signed APK to `output`.
    #[must_use]
    pub fn new(sdk: AndroidSdk, staging: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
        Self {
            sdk,
            staging: simplify(&staging.into()),
            output: simplify(&output.into()),
            min_sdk: ANDROID_MIN_SDK,
            target_sdk: ANDROID_TARGET_SDK,
            signing: ApkSigning::Debug,
        }
    }

    /// Override the `min`/`target` SDK versions aapt2 stamps.
    #[must_use]
    pub fn with_sdk_versions(mut self, min: u32, target: u32) -> Self {
        self.min_sdk = min;
        self.target_sdk = target;
        self
    }

    /// Use a specific signing identity (default: a generated debug keystore).
    #[must_use]
    pub fn with_signing(mut self, signing: ApkSigning) -> Self {
        self.signing = signing;
        self
    }

    /// Run the full pipeline and return the path to the signed `.apk`.
    ///
    /// # Errors
    ///
    /// [`ApkError`] for a missing input, a filesystem failure, or any tool that
    /// cannot be spawned or exits non-zero (fail-closed — no partial APK is
    /// returned).
    pub fn assemble(&self) -> Result<PathBuf, ApkError> {
        let manifest = self.staging.join("AndroidManifest.xml");
        if !manifest.is_file() {
            return Err(ApkError::MissingInput {
                what: "AndroidManifest.xml".to_owned(),
                path: manifest,
            });
        }
        let assets = self.staging.join("assets");
        let jni_libs = self.staging.join("jniLibs").join(APK_ABI);

        // Clean scratch workspace inside the staging dir.
        let work = self.staging.join(".apk-build");
        if work.exists() {
            std::fs::remove_dir_all(&work).map_err(|source| ApkError::Io {
                path: work.clone(),
                source,
            })?;
        }
        mkdir(&work)?;

        // Remap jniLibs/<abi> -> lib/<abi> (APK on-device convention), where
        // `jar uf -C . lib` will pick them up as `lib/arm64-v8a/*.so`.
        let lib_abi = work.join(APK_LIB_DIR).join(APK_ABI);
        mkdir(&lib_abi)?;
        let staged_libs = copy_dir_contents(&jni_libs, &lib_abi)?;
        if staged_libs == 0 {
            return Err(ApkError::MissingInput {
                what: "native libraries (jniLibs/arm64-v8a/*.so)".to_owned(),
                path: jni_libs,
            });
        }

        // 1. aapt2 link: binary manifest + resources.arsc + bundled assets.
        let unaligned = work.join("unaligned.apk");
        let link = aapt2_link_args(
            &self.sdk.android_jar,
            &manifest,
            assets.is_dir().then_some(assets.as_path()),
            self.min_sdk,
            self.target_sdk,
            &unaligned,
        );
        run_tool("aapt2", &self.sdk.aapt2, &link, None)?;

        // 2. jar uf: insert the native libs under lib/ (deflated; the manifest
        //    sets android:extractNativeLibs="true" so the installer extracts).
        let jar_args = jar_update_args(&unaligned, APK_LIB_DIR);
        run_tool("jar", &self.sdk.jar, &jar_args, Some(&work))?;

        // 3. zipalign -f 4.
        let aligned = work.join("aligned.apk");
        let za = zipalign_args(&unaligned, &aligned);
        run_tool("zipalign", &self.sdk.zipalign, &za, None)?;

        // 4. resolve a signing identity (generate a debug keystore if needed).
        let (ks_path, store_pass, alias, key_pass) = self.resolve_signing(&work)?;

        // 5. apksigner sign -> a temp inside the work dir (NOT self.output), so a
        //    failed sign/verify never leaves a stale or unverified APK at the
        //    canonical output path a packaging pipeline consumes.
        let signed = work.join("signed.apk");
        let sign = apksigner_sign_args(&ks_path, &alias, &signed, &aligned);
        // Pass the keystore/key passwords to apksigner via its environment rather
        // than `pass:<literal>` on argv: argv is visible on the process table to
        // other users, and on Windows a literal `%` in the password is mangled by
        // cmd.exe batch escaping (apksigner is a .bat). `env:NAME` sidesteps both.
        let env = [
            (APKSIGNER_STORE_PASS_ENV, store_pass.as_str()),
            (APKSIGNER_KEY_PASS_ENV, key_pass.as_str()),
        ];
        // apksigner is a wrapper script that re-resolves `java` from PATH; point
        // it at the JDK we validated so it never falls back to a launcher stub.
        let (path_override, java_home) = self.sdk.apksigner_env();
        let mut sign_env: Vec<(&str, &str)> = env.to_vec();
        if let Some(ref p) = path_override {
            sign_env.push(("PATH", p.as_str()));
        }
        if let Some(ref jh) = java_home {
            sign_env.push(("JAVA_HOME", jh.as_str()));
        }
        run_tool_env("apksigner", &self.sdk.apksigner, &sign, None, &sign_env)?;

        // 6. apksigner verify the signed temp (fail-closed) BEFORE publishing it.
        let verify = apksigner_verify_args(&signed);
        let mut verify_env: Vec<(&str, &str)> = Vec::new();
        if let Some(ref p) = path_override {
            verify_env.push(("PATH", p.as_str()));
        }
        if let Some(ref jh) = java_home {
            verify_env.push(("JAVA_HOME", jh.as_str()));
        }
        run_tool_env("apksigner", &self.sdk.apksigner, &verify, None, &verify_env)?;

        // 7. Publish: atomically replace any prior output with the freshly-
        //    verified APK, then drop the scratch workspace (incl. the generated
        //    debug keystore and the duplicated native libs). `publish` never
        //    deletes or truncates the prior output until the new APK is fully in
        //    place, so a failed publish leaves a previously-good APK intact
        //    rather than destroying it. Cleanup is best-effort — a cleanup
        //    failure must not fail an otherwise-good build.
        if let Some(parent) = self.output.parent() {
            mkdir(parent)?;
        }
        publish(&signed, &self.output)?;
        let _ = std::fs::remove_dir_all(&work);

        Ok(self.output.clone())
    }

    /// Resolve the keystore path + credentials, generating a debug keystore in
    /// `work` when the identity is [`ApkSigning::Debug`].
    fn resolve_signing(&self, work: &Path) -> Result<(PathBuf, String, String, String), ApkError> {
        match &self.signing {
            ApkSigning::Keystore {
                path,
                store_pass,
                key_alias,
                key_pass,
            } => Ok((
                simplify(path),
                store_pass.clone(),
                key_alias.clone(),
                key_pass.clone(),
            )),
            ApkSigning::Debug => {
                let ks = work.join("debug.jks");
                let args = keytool_debug_args(&ks);
                run_tool("keytool", &self.sdk.keytool, &args, None)?;
                Ok((
                    ks,
                    "android".to_owned(),
                    "androiddebugkey".to_owned(),
                    "android".to_owned(),
                ))
            }
        }
    }
}

/// Run one SDK/JDK tool, capturing output and failing closed on a non-zero exit.
fn run_tool(
    tool: &str,
    program: &Path,
    args: &[String],
    cwd: Option<&Path>,
) -> Result<(), ApkError> {
    run_tool_env(tool, program, args, cwd, &[])
}

/// [`run_tool`] with extra environment variables set on the child only (used to
/// hand apksigner its passwords via `env:` without putting them on the argv /
/// process table or exporting them into this process's global environment).
fn run_tool_env(
    tool: &str,
    program: &Path,
    args: &[String],
    cwd: Option<&Path>,
    env: &[(&str, &str)],
) -> Result<(), ApkError> {
    let _timer = StepTimer::start(tool);
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd.output().map_err(|source| ApkError::Spawn {
        tool: tool.to_owned(),
        source,
    })?;
    if !output.status.success() {
        return Err(ApkError::CommandFailed {
            tool: tool.to_owned(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

// ── pure argument builders (unit-tested without running the tools) ──────────

/// `aapt2 link` arguments: include the platform jar, compile the manifest,
/// bundle the assets dir (when present), stamp min/target SDK, emit `out`.
#[must_use]
pub fn aapt2_link_args(
    android_jar: &Path,
    manifest: &Path,
    assets: Option<&Path>,
    min_sdk: u32,
    target_sdk: u32,
    out: &Path,
) -> Vec<String> {
    let mut args = vec![
        "link".to_owned(),
        "-I".to_owned(),
        android_jar.display().to_string(),
        "--manifest".to_owned(),
        manifest.display().to_string(),
    ];
    if let Some(assets) = assets {
        args.push("-A".to_owned());
        args.push(assets.display().to_string());
    }
    args.push("--min-sdk-version".to_owned());
    args.push(min_sdk.to_string());
    args.push("--target-sdk-version".to_owned());
    args.push(target_sdk.to_string());
    args.push("-o".to_owned());
    args.push(out.display().to_string());
    args
}

/// `jar uf <apk> -C . <lib-dir>` — insert the `lib/` subtree into the APK zip.
/// Run with the current dir set to the workdir so `-C .` resolves to it.
#[must_use]
pub fn jar_update_args(apk: &Path, lib_dir: &str) -> Vec<String> {
    vec![
        "uf".to_owned(),
        apk.display().to_string(),
        "-C".to_owned(),
        ".".to_owned(),
        lib_dir.to_owned(),
    ]
}

/// `zipalign -f 4 <input> <output>`.
#[must_use]
pub fn zipalign_args(input: &Path, output: &Path) -> Vec<String> {
    vec![
        "-f".to_owned(),
        "4".to_owned(),
        input.display().to_string(),
        output.display().to_string(),
    ]
}

/// `keytool -genkeypair` for a self-signed debug key (`androiddebugkey`,
/// RSA-2048, 10000-day validity, pass `android`).
#[must_use]
pub fn keytool_debug_args(keystore: &Path) -> Vec<String> {
    vec![
        "-genkeypair".to_owned(),
        "-v".to_owned(),
        "-keystore".to_owned(),
        keystore.display().to_string(),
        "-storepass".to_owned(),
        "android".to_owned(),
        "-keypass".to_owned(),
        "android".to_owned(),
        "-alias".to_owned(),
        "androiddebugkey".to_owned(),
        "-keyalg".to_owned(),
        "RSA".to_owned(),
        "-keysize".to_owned(),
        "2048".to_owned(),
        "-validity".to_owned(),
        "10000".to_owned(),
        "-dname".to_owned(),
        "CN=Android Debug,O=Android,C=US".to_owned(),
    ]
}

/// Environment variable apksigner reads the keystore password from (`env:` ref).
pub const APKSIGNER_STORE_PASS_ENV: &str = "IDTX_APKSIGNER_KS_PASS";
/// Environment variable apksigner reads the key password from (`env:` ref).
pub const APKSIGNER_KEY_PASS_ENV: &str = "IDTX_APKSIGNER_KEY_PASS";

/// `apksigner sign` arguments. Passwords are **not** here: they are passed via
/// the process environment ([`APKSIGNER_STORE_PASS_ENV`] /
/// [`APKSIGNER_KEY_PASS_ENV`]) and referenced as `env:NAME`, keeping the secret
/// off the process table and clear of cmd.exe `%`-escaping on Windows.
#[must_use]
pub fn apksigner_sign_args(
    keystore: &Path,
    key_alias: &str,
    out: &Path,
    input: &Path,
) -> Vec<String> {
    vec![
        "sign".to_owned(),
        "--ks".to_owned(),
        keystore.display().to_string(),
        "--ks-pass".to_owned(),
        format!("env:{APKSIGNER_STORE_PASS_ENV}"),
        "--ks-key-alias".to_owned(),
        key_alias.to_owned(),
        "--key-pass".to_owned(),
        format!("env:{APKSIGNER_KEY_PASS_ENV}"),
        "--out".to_owned(),
        out.display().to_string(),
        input.display().to_string(),
    ]
}

/// `apksigner verify --print-certs <apk>`.
#[must_use]
pub fn apksigner_verify_args(apk: &Path) -> Vec<String> {
    vec![
        "verify".to_owned(),
        "--print-certs".to_owned(),
        apk.display().to_string(),
    ]
}

// ── discovery helpers ───────────────────────────────────────────────────────

fn android_sdk_root() -> Option<PathBuf> {
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    // Per-OS default install location.
    let default = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Android").join("Sdk"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Android").join("sdk"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Android").join("Sdk"))
    };
    default.filter(|p| p.is_dir())
}

/// Resolve the Android SDK root, preferring an explicit `override` (from
/// `[build.android].sdk`) over the environment. `discover_with` has already
/// rejected a non-directory override up-front (as
/// [`ApkError::ConfiguredPathNotFound`]); the `is_dir` guard here is a defensive
/// secondary check, so a bad override still resolves to `None` rather than a
/// surprise fallback.
fn resolve_sdk_root(override_root: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = override_root {
        return root.is_dir().then(|| root.to_path_buf());
    }
    android_sdk_root()
}

/// Locate a JDK tool (`jar`, `keytool`), preferring an explicit JDK root
/// (`[build.android].jdk`), then `JAVA_HOME`, then the parent of a `java` on
/// `PATH`, and finally the tool directly on `PATH`.
///
/// Resolving each tool independently (rather than demanding both sit beside a
/// single resolved `java`) tolerates the Windows Oracle `javapath` launcher
/// shim — a `java.exe` with no `jar`/`keytool` beside it — by falling through to
/// a full JDK reachable another way.
fn jdk_tool(base: &str, jdk_override: Option<&Path>) -> Option<PathBuf> {
    let mut bins: Vec<PathBuf> = Vec::new();
    if let Some(jdk) = jdk_override {
        bins.push(jdk.join("bin"));
    }
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        bins.push(PathBuf::from(home).join("bin"));
    }
    if let Some(java_dir) = which("java").and_then(|p| p.parent().map(Path::to_path_buf)) {
        bins.push(java_dir);
    }
    bins.into_iter()
        .filter(|d| d.is_dir())
        .find_map(|d| tool_in(&d, base))
        .or_else(|| which(base))
}

/// Find an executable `base` within `dir`, trying the host's extensions.
fn tool_in(dir: &Path, base: &str) -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        // apksigner/keytool are batch scripts; aapt2/zipalign/jar are .exe.
        [
            format!("{base}.exe"),
            format!("{base}.bat"),
            format!("{base}.cmd"),
            base.to_owned(),
        ]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
    } else {
        // On Unix a tool is only usable if it is a regular file with the execute
        // bit set — a non-executable same-named file must not shadow a real tool
        // further down the search path.
        let p = dir.join(base);
        is_executable_file(&p).then_some(p)
    }
}

/// Whether `p` is a regular file with any execute bit set (Unix). On non-Unix
/// this is just an `is_file` check (handled by callers / `tool_in`).
#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

/// Locate `name` on `PATH` (with host extensions).
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| tool_in(&dir, name))
}

/// Pick the subdirectory of `root` whose name sorts highest under `key`
/// (returns the directory *name*, not the full path).
fn highest_subdir(root: &Path, key: impl Fn(&str) -> Option<Vec<u32>>) -> Option<String> {
    let mut best: Option<(Vec<u32>, String)> = None;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(v) = key(&name) {
            match &best {
                Some((bv, _)) if *bv >= v => {}
                _ => best = Some((v, name)),
            }
        }
    }
    best.map(|(_, name)| name)
}

/// `"36.1.0"` -> `[36, 1, 0]`; non-version names yield `None`.
fn parse_dotted_version(name: &str) -> Option<Vec<u32>> {
    let parts: Vec<u32> = name.split('.').filter_map(|p| p.parse().ok()).collect();
    (!parts.is_empty() && parts.len() == name.split('.').count()).then_some(parts)
}

/// `"android-36"` -> `[36]`; `"android-37.0"` -> `[37, 0]`; anything else yields
/// `None`.
///
/// Recent SDK installs name platform directories with a dotted minor version
/// (`android-36.1`, `android-37.0`) alongside the historical plain
/// `android-NN`. Parse the suffix as a dotted version so both forms resolve and
/// still order correctly (`android-37.0` > `android-36.1` > `android-36`).
fn parse_platform_version(name: &str) -> Option<Vec<u32>> {
    name.strip_prefix("android-").and_then(parse_dotted_version)
}

// ── fs helpers (ApkError domain) ────────────────────────────────────────────

/// Strip a Windows extended-length / verbatim prefix (`\\?\` or `\\?\UNC\`) from
/// a path so the JDK tools (`jar`, `keytool`) accept it.
///
/// `std::fs::canonicalize` on Windows returns verbatim paths; native tools
/// (aapt2/zipalign) tolerate them, but the JDK's `java.io.File` → `WindowsPath`
/// parser rejects the `?` with `InvalidPathException: Illegal character [?] in
/// path at index 2`, which is what breaks `jar uf` on a canonicalized staging
/// dir. On non-Windows this is a no-op.
#[cfg(windows)]
fn simplify(p: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    // Strip on the raw UTF-16 units rather than via a lossy `String` round-trip:
    // a path can carry a non-Unicode component (an unpaired surrogate in the
    // WTF-8 `OsStr`) that `to_string_lossy` would replace with U+FFFD, leaving
    // the simplified path pointing at a different, nonexistent location.
    let units: Vec<u16> = p.as_os_str().encode_wide().collect();
    let verbatim_unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    if units.starts_with(&verbatim_unc) {
        // `\\?\UNC\server\share` -> `\\server\share`
        let mut out: Vec<u16> = r"\\".encode_utf16().collect();
        out.extend_from_slice(&units[verbatim_unc.len()..]);
        PathBuf::from(OsString::from_wide(&out))
    } else if units.starts_with(&verbatim) {
        PathBuf::from(OsString::from_wide(&units[verbatim.len()..]))
    } else {
        p.to_path_buf()
    }
}

#[cfg(not(windows))]
fn simplify(p: &Path) -> PathBuf {
    p.to_path_buf()
}

/// Atomically place `src` at `dst`, overwriting any existing file there, without
/// ever leaving a truncated or missing file at `dst` on failure.
///
/// A plain `rename` is tried first — atomic and overwriting on both Windows
/// (`MOVEFILE_REPLACE_EXISTING`) and Unix when `src`/`dst` share a volume. When
/// that fails (commonly `CrossesDevices`: the scratch workspace in `%TEMP%` on a
/// different volume than the output), the APK is copied to a temporary sibling
/// *in the output directory* and that is renamed into place. The copy therefore
/// never touches `dst` directly: a mid-copy failure (disk full, dropped share,
/// AV lock) leaves only the discarded temp, and the prior `dst` — if any — is
/// still intact and verified. Maps any failure into [`ApkError::Io`].
fn publish(src: &Path, dst: &Path) -> Result<(), ApkError> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    // Cross-volume (or otherwise un-renamable): stage a copy beside `dst` so the
    // canonical output path is only ever updated by an atomic same-volume
    // rename, never by a partial copy.
    let tmp = dst.with_extension("apk.publish-tmp");
    let _ = std::fs::remove_file(&tmp);
    if let Err(source) = std::fs::copy(src, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ApkError::Io { path: tmp, source });
    }
    if let Err(source) = std::fs::rename(&tmp, dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ApkError::Io {
            path: dst.to_path_buf(),
            source,
        });
    }
    let _ = std::fs::remove_file(src);
    Ok(())
}

fn mkdir(dir: &Path) -> Result<(), ApkError> {
    std::fs::create_dir_all(dir).map_err(|source| ApkError::Io {
        path: dir.to_path_buf(),
        source,
    })
}

/// Copy every regular file directly inside `from` into `to` (non-recursive —
/// native libs are flat under the ABI dir). Returns the count copied; a missing
/// source dir yields `0`.
fn copy_dir_contents(from: &Path, to: &Path) -> Result<usize, ApkError> {
    if !from.is_dir() {
        return Ok(0);
    }
    let mut copied = 0;
    for entry in std::fs::read_dir(from)
        .map_err(|source| ApkError::Io {
            path: from.to_path_buf(),
            source,
        })?
        .flatten()
    {
        let src = entry.path();
        if src.is_file() {
            let dst = to.join(entry.file_name());
            std::fs::copy(&src, &dst).map_err(|source| ApkError::Io { path: dst, source })?;
            copied += 1;
        }
    }
    Ok(copied)
}

// ── optional per-step timing (CLAUDE.md: perf behind a feature) ─────────────

/// Times one assembly step and logs the elapsed ms on drop when the
/// `cook-metrics` feature is on; a zero-cost no-op otherwise.
struct StepTimer {
    #[cfg(feature = "cook-metrics")]
    label: String,
    #[cfg(feature = "cook-metrics")]
    start: std::time::Instant,
}

impl StepTimer {
    fn start(label: &str) -> Self {
        #[cfg(not(feature = "cook-metrics"))]
        let _ = label;
        Self {
            #[cfg(feature = "cook-metrics")]
            label: label.to_owned(),
            #[cfg(feature = "cook-metrics")]
            start: std::time::Instant::now(),
        }
    }
}

#[cfg(feature = "cook-metrics")]
impl Drop for StepTimer {
    fn drop(&mut self) {
        tracing::info!(
            step = self.label.as_str(),
            elapsed_ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX),
            "apk assembly step"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aapt2_link_args_shape() {
        let args = aapt2_link_args(
            Path::new("/sdk/android.jar"),
            Path::new("/stage/AndroidManifest.xml"),
            Some(Path::new("/stage/assets")),
            30,
            32,
            Path::new("/work/unaligned.apk"),
        );
        assert_eq!(args[0], "link");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-I" && w[1] == "/sdk/android.jar")
        );
        assert!(args.windows(2).any(|w| w[0] == "--manifest"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-A" && w[1] == "/stage/assets")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--min-sdk-version" && w[1] == "30")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--target-sdk-version" && w[1] == "32")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-o" && w[1] == "/work/unaligned.apk")
        );
    }

    #[test]
    fn aapt2_link_args_omit_assets_when_absent() {
        let args = aapt2_link_args(
            Path::new("a.jar"),
            Path::new("m.xml"),
            None,
            30,
            32,
            Path::new("o.apk"),
        );
        assert!(!args.iter().any(|a| a == "-A"));
    }

    #[test]
    fn jar_update_args_target_lib() {
        let args = jar_update_args(Path::new("/w/unaligned.apk"), "lib");
        assert_eq!(args, vec!["uf", "/w/unaligned.apk", "-C", ".", "lib"]);
    }

    #[test]
    fn zipalign_args_force_4() {
        let args = zipalign_args(Path::new("in.apk"), Path::new("out.apk"));
        assert_eq!(args, vec!["-f", "4", "in.apk", "out.apk"]);
    }

    #[test]
    fn apksigner_sign_args_wrap_passwords() {
        let args = apksigner_sign_args(
            Path::new("/w/debug.jks"),
            "androiddebugkey",
            Path::new("/out/app.apk"),
            Path::new("/w/aligned.apk"),
        );
        assert_eq!(args[0], "sign");
        // Passwords are passed via the environment (env: refs), never on argv —
        // keeps the secret off the process table and clear of cmd.exe %-escaping.
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--ks-pass" && w[1] == format!("env:{APKSIGNER_STORE_PASS_ENV}"))
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--key-pass" && w[1] == format!("env:{APKSIGNER_KEY_PASS_ENV}"))
        );
        assert!(!args.iter().any(|a| a.starts_with("pass:")));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--ks-key-alias" && w[1] == "androiddebugkey")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--out" && w[1] == "/out/app.apk")
        );
        assert_eq!(args.last().unwrap(), "/w/aligned.apk");
    }

    #[test]
    fn simplify_strips_verbatim_prefix() {
        // No-op on non-verbatim input, on every platform.
        assert_eq!(
            simplify(Path::new("relative/path")),
            PathBuf::from("relative/path")
        );
        #[cfg(windows)]
        {
            assert_eq!(simplify(Path::new(r"\\?\C:\a\b")), PathBuf::from(r"C:\a\b"));
            assert_eq!(
                simplify(Path::new(r"\\?\UNC\server\share\x")),
                PathBuf::from(r"\\server\share\x")
            );
            // A plain drive path is already fine — unchanged.
            assert_eq!(simplify(Path::new(r"C:\a\b")), PathBuf::from(r"C:\a\b"));
        }
    }

    #[test]
    fn discover_with_rejects_mistyped_sdk_override() {
        // A configured sdk path that is not a directory is a user typo, not the
        // benign "no SDK on this host" case — it must hard-error (named path),
        // not fall back to the environment.
        let bogus = std::env::temp_dir().join("idtx-no-such-sdk-xyz");
        let _ = std::fs::remove_dir_all(&bogus);
        match AndroidSdk::discover_with(Some(&bogus), None) {
            Err(ApkError::ConfiguredPathNotFound { what, path }) => {
                assert_eq!(what, "sdk");
                assert_eq!(path, bogus);
            }
            other => panic!("expected ConfiguredPathNotFound(sdk), got {other:?}"),
        }
    }

    #[test]
    fn discover_with_rejects_mistyped_jdk_override() {
        // The jdk override gets the same reject-on-mistype contract as sdk; a
        // bad jdk must not silently fall through to JAVA_HOME/PATH. Checked
        // up-front (sdk=None) so this is deterministic regardless of host SDK.
        let bogus = std::env::temp_dir().join("idtx-no-such-jdk-xyz");
        let _ = std::fs::remove_dir_all(&bogus);
        match AndroidSdk::discover_with(None, Some(&bogus)) {
            Err(ApkError::ConfiguredPathNotFound { what, path }) => {
                assert_eq!(what, "jdk");
                assert_eq!(path, bogus);
            }
            other => panic!("expected ConfiguredPathNotFound(jdk), got {other:?}"),
        }
    }

    #[test]
    fn publish_replaces_existing_output() {
        let dir = std::env::temp_dir().join("idtx-publish-replace");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("signed.apk");
        let dst = dir.join("app.apk");
        std::fs::write(&src, b"NEW").unwrap();
        std::fs::write(&dst, b"OLD").unwrap();
        publish(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"NEW");
        assert!(!src.exists(), "src consumed on publish");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publish_preserves_prior_output_on_failure() {
        // Fail-closed: if the new APK cannot be published, a previously-good
        // output must survive intact rather than being deleted/truncated.
        let dir = std::env::temp_dir().join("idtx-publish-failclosed");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let missing_src = dir.join("does-not-exist.apk");
        let dst = dir.join("app.apk");
        std::fs::write(&dst, b"GOOD").unwrap();
        assert!(
            publish(&missing_src, &dst).is_err(),
            "publishing a missing src must fail"
        );
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            b"GOOD",
            "prior output left intact on publish failure"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apksigner_verify_args_print_certs() {
        let args = apksigner_verify_args(Path::new("app.apk"));
        assert_eq!(args, vec!["verify", "--print-certs", "app.apk"]);
    }

    #[test]
    fn keytool_debug_args_self_signed() {
        let args = keytool_debug_args(Path::new("/w/debug.jks"));
        assert_eq!(args[0], "-genkeypair");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-alias" && w[1] == "androiddebugkey")
        );
        assert!(args.windows(2).any(|w| w[0] == "-keyalg" && w[1] == "RSA"));
    }

    #[test]
    fn build_tools_version_sort() {
        assert_eq!(parse_dotted_version("36.1.0"), Some(vec![36, 1, 0]));
        assert_eq!(parse_dotted_version("34.0.0"), Some(vec![34, 0, 0]));
        assert_eq!(parse_dotted_version("not-a-version"), None);
        // Higher major beats lower, and the dotted compare is lexicographic by
        // component (Vec<u32> Ord), so 36.1.0 > 36.0.3 > 34.0.0.
        assert!(parse_dotted_version("36.1.0") > parse_dotted_version("36.0.3"));
        assert!(parse_dotted_version("36.0.3") > parse_dotted_version("34.0.0"));
    }

    #[test]
    fn platform_version_parse() {
        assert_eq!(parse_platform_version("android-36"), Some(vec![36]));
        assert_eq!(parse_platform_version("android-30"), Some(vec![30]));
        // Newer SDK installs use dotted minor-version directory names.
        assert_eq!(parse_platform_version("android-37.0"), Some(vec![37, 0]));
        assert_eq!(parse_platform_version("android-36.1"), Some(vec![36, 1]));
        assert_eq!(parse_platform_version("data"), None);
        assert_eq!(parse_platform_version("android-"), None);
        assert_eq!(parse_platform_version("android-Baklava"), None);
        assert!(parse_platform_version("android-36") > parse_platform_version("android-30"));
        // Dotted forms order correctly against each other and the plain form.
        assert!(parse_platform_version("android-37.0") > parse_platform_version("android-36.1"));
        assert!(parse_platform_version("android-36.1") > parse_platform_version("android-36"));
    }

    #[test]
    fn signing_defaults_to_debug() {
        assert!(matches!(ApkSigning::default(), ApkSigning::Debug));
    }

    /// End-to-end assembly against the **real** Android SDK, gated on its
    /// presence (CLAUDE.md tool-bound discipline): on a host without the SDK it
    /// skips cleanly; on a host with it, it drives aapt2 → jar → zipalign →
    /// keytool → apksigner and asserts `apksigner verify` passes.
    #[test]
    fn assembles_and_verifies_a_real_apk_when_sdk_present() {
        let Ok(sdk) = AndroidSdk::discover() else {
            eprintln!("skipping: no Android SDK (set ANDROID_HOME) — tool-bound test");
            return;
        };

        let tmp = std::env::temp_dir().join("idtx-apk-assembler-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let staging = tmp.join("stage");
        let jni = staging.join("jniLibs").join("arm64-v8a");
        let assets = staging.join("assets");
        std::fs::create_dir_all(&jni).unwrap();
        std::fs::create_dir_all(&assets).unwrap();
        // Link the REAL shipped manifest template (placeholders substituted) so
        // aapt2 validates the exact bytes a Quest build emits — this is what
        // would have caught the `--`-in-comment "not well-formed" regression.
        // The template deliberately carries no `<uses-sdk>`; aapt2 synthesizes
        // one from the assembler's --min/--target-sdk-version flags.
        let manifest = include_str!("../templates/android/AndroidManifest.xml")
            .replace("{{PACKAGE}}", "com.idtx.assemblertest")
            .replace("{{LABEL}}", "AssemblerTest")
            .replace("{{LIB_NAME}}", "idtx_runtime");
        std::fs::write(staging.join("AndroidManifest.xml"), manifest).unwrap();
        // A stand-in native lib + a payload asset — the pipeline treats both as
        // opaque bytes.
        std::fs::write(jni.join("libidtx_runtime.so"), b"\x7fELF stub bytes").unwrap();
        std::fs::write(assets.join("run.toml"), b"target = \"quest\"\n").unwrap();

        let out = tmp.join("app.apk");
        // Pass a *canonicalized* staging path: on Windows that yields a `\\?\`
        // verbatim prefix, which is exactly what broke the JDK `jar` step
        // (`InvalidPathException: Illegal character [?]`) in the field. Assembling
        // from it exercises the `simplify()` fix end-to-end against the real tools.
        let staging_canon = std::fs::canonicalize(&staging).unwrap_or(staging.clone());
        let apk = ApkAssembler::new(sdk, &staging_canon, &out)
            .assemble()
            .expect("apk assembly");
        assert!(apk.is_file(), "apk should exist at {}", apk.display());
        assert!(
            std::fs::metadata(&apk).unwrap().len() > 0,
            "apk should be non-empty"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
