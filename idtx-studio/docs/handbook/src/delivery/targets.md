# Prepare and validate target devices

The USD and behavior source stay in your project. Each target uses a different supplied native Runtime and application layout. Validate Signal Room as a desktop package first, then build and test it on the intended device.

## What you install, and what the distribution supplies

The [setup chapter](../setup.md) covers the Rust/Cargo tools and guest target used to compile behaviors. Native rendering/physics/USD libraries and target Runtime binaries come from the Conductor distribution. Obtain a package that includes the targets you need; a desktop-only distribution is insufficient for Quest or Vision Pro.

| Target | Supplied package content | Additional tools or device setup |
|---|---|---|
| Windows desktop | Host Runtime, native libraries and USD/resource trees. | Compatible Windows machine and graphics driver. |
| macOS desktop | Host Runtime and frameworks/resources in the authoring app. | Compatible macOS machine. |
| Linux desktop | Compatible host Runtime and native resources, when your supplier provides this distribution. | Compatible Linux machine and GPU/backend. |
| Windows PCVR | Windows Runtime with `pcvr` support. | Active OpenXR runtime, connected headset and working controller input. |
| Quest | Android ARM64 runtime, OpenXR loader, native dependencies and USD/plugins. | Android SDK build-tools/platform, JDK and `adb`; headset Developer Mode. |
| Vision Pro | visionOS ARM64 device runtime, native dependencies and USD/plugins. | macOS with Xcode/visionOS SDK, Apple development signing and paired device. |

The implemented PCVR presentation path is Windows OpenXR with D3D12. A Linux PCVR cook profile exists, but its OpenXR Vulkan session is not implemented; macOS desktop support also does not establish a macOS PCVR backend. Use Windows for this book's PCVR workflow.

If a build reports missing native runtime files, architecture/ABI mismatch or missing features, obtain a compatible target-enabled distribution. Adding a target to the manifest cannot supply a native binary. Keep the installed application layout intact; moving its executable alone breaks dependency discovery.

## Desktop and PCVR

Choose **Build > Build Desktop Experience** for the host OS. On Windows/Linux, launch the runtime in the finished `Signal Room/` folder; on macOS, open `Signal Room.app`. The [cooking chapter](cooking.md) shows the output structure and relocation test.

Cooked desktop presentation defaults to fullscreen with cursor capture. Conductor previews run windowed. Test movement, pointer actions and both transitions in the cooked package, since it exercises the level registry that an active-level preview does not.

For PCVR:

1. Install and activate the OpenXR runtime supplied by your headset platform.
2. Connect the headset and verify it works in that platform's own application.
3. Confirm your Conductor distribution includes a PCVR-enabled Windows Runtime.
4. Choose **Build > Build PCVR Experience**, then launch its finished package.
5. Confirm XR presentation, both hands/controller pointers, logical bindings, metre scale and level Spawn placement.

The same behavior can read logical actions on desktop and XR, but device bindings and ergonomics require testing. Signal Room's own pointer picks along the active camera ray: face the console, then use the mapped controller button or a supported pinch action to click. Its input map uses `Controller/MetaQuest/...` source names. Controller-model overrides change appearance and mounting; they do not create new input bindings. A successful desktop window does not establish an active XR session.

The generated macOS desktop experience app is not distribution-signed or notarized by the current experience packager. Apply your organization's macOS release/signing process when preparing public distribution; the local build remains useful for development acceptance.

## Quest: verify the supplied runtime

Your authoring package must provide this tree beside the Conductor executable. On macOS, that executable is inside `Contents/MacOS/`:

```text
android-runtime/
  arm64-v8a/
    libidtx_runtime.so
    libopenxr_loader.so
    ...ARM64 native dependencies...
  usd/...
  usd-exec/...                     # only for an OpenExec-enabled runtime
  idtx-http-resolver/...           # only for a remote-enabled runtime
```

The Android Runtime and all dependencies must be genuine Android ARM64 artifacts from the matching release. You do not need an Android NDK or native Rust Android toolchain to cook with these prebuilt artifacts. Behaviors use the same WASM guest target as desktop.

## Quest: prepare APK tools

Install an Android SDK through Android Studio's SDK Manager or the Android command-line tools. Include a platform containing `android.jar` (Android API 32 or later), stable SDK Build-Tools and Platform-Tools. Install a full JDK compatible with those tools, for example JDK 17.

Conductor needs `aapt2`, `zipalign` and `apksigner` from SDK Build-Tools, plus `jar` and `keytool` from the JDK. `adb` is used for device installation. A Java launcher without the JDK tools is insufficient.

Set **Project > Settings… > Cooking > Android SDK** and **JDK**, or edit the source manifest:

```toml
[build.android]
sdk = "C:/Android/Sdk"
jdk = "C:/Java/jdk-17"
texture_quality = "authoring"
```

Replace these paths with existing installations. Conductor can also discover the SDK through `ANDROID_HOME`/`ANDROID_SDK_ROOT` and the JDK through `JAVA_HOME`/`PATH`. It selects the highest-versioned installed Build-Tools and Android platform. The **Android NDK** setting records native-build configuration; it is not needed for assembling this prebuilt runtime's APK.

## Quest: cook, assemble and install

Choose **Build > Build Quest Experience**. Conductor validates the Android Runtime, copies the full payload into APK assets and stages native libraries under `jniLibs/arm64-v8a`. It also writes an asset inventory for on-device extraction.

With SDK/JDK tools available, Conductor assembles and signs `build/quest/Signal Room.apk` directly. If no SDK is discovered, it reports a **staging tree only** result. Install/configure the SDK and rebuild to produce an installable APK. An explicitly invalid SDK/JDK path or a failing tool is a build error. Some versions retain an older menu tooltip referring to a separate assembler; the completion result describes the actual output.

The current APK uses minimum Android API 30 and target API 32 and is debug-signed for development sideloading. Store release requirements and release signing are outside this development package flow.

Enable Developer Mode on the Quest, connect it and authorize USB debugging. From the project root:

```powershell
adb devices
adb install -r "./build/quest/Signal Room.apk"
```

Use the `adb` executable in your Platform-Tools installation if it is not on `PATH`. Start the installed app from the headset. Conductor currently has no Quest **Build and deploy** action. Use `adb logcat` to investigate startup or behavior failures.

Check boot, resource resolution, both controllers, console actions, both level transitions and peak memory during the swap. Install and test the completed APK rather than the author's desktop preview.

## Vision Pro: prepare the supplied runtime and Apple tools

Use a macOS Conductor distribution containing a device runtime beside the executable:

```text
visionos-runtime/
  arm64/
    libidtx_runtime.dylib
    ...device native dependencies...
  usd/...
  usd-exec/...                     # only when enabled
```

The physical-device slice is `arm64`; an `arm64-sim` simulator slice is a different target. Obtain the device package for this walkthrough. Cooking with a prebuilt runtime does not require a nightly Rust toolchain or building native USD libraries.

Install Xcode with the visionOS SDK, select its developer tools, enable Developer Mode on Vision Pro and pair the device through Xcode. Add your Apple account and development team. Device signing requires a development identity and provisioning profile that covers the experience's bundle ID and paired device. The current build supports Personal-Team development signing.

Configure the project through **Project > Settings… > Cooking > VisionOS**, or use these fields:

```toml
[build.visionos]
bundle_id = "com.example.signalroom"
identity = "Apple Development: Your Name (TEAMID)"
provisioning_profile = "/Users/you/Library/MobileDevice/Provisioning Profiles/signalroom.mobileprovision"
team_id = "TEAMID"
enable_hand_tracking = true
texture_quality = "authoring"
```

Set actual values for your team/profile, and choose a bundle ID you can sign. Identity and profile may be omitted for discovery. Conductor can attempt a signing bootstrap using Xcode; if your team needs one-time interactive setup, create/open a small visionOS app in Xcode using the same bundle ID, select your team with automatic signing and run it on the paired device once. Then rebuild the experience in Conductor. The native scaffold is for provisioning your Apple signing environment; your experience remains the USD project.

## Vision Pro: build and deploy

Choose **Build > Build Vision OS > Build only** to create the app on disk. With Xcode and valid credentials, Conductor compiles the supplied Swift shell and signs the device app. The finished layout is:

```text
Signal Room.app/
  Signal Room                     # compiled native app shell
  Info.plist
  embedded.mobileprovision
  run.toml
  levels/...
  Frameworks/
    libidtx_runtime.dylib
    ...native dependencies and USD plugin resources...
  ...remaining payload resources...
```

Without the macOS toolchain/signing credentials, a build can leave an **unsigned staged app** and report that limitation. An `.app` folder alone does not prove it is installable. Complete Apple setup and rebuild on macOS.

**Build > Build Vision OS > Build and deploy** becomes available when Conductor detects a paired Vision Pro. It installs and launches the signed app using Xcode's `devicectl`. A deployment failure can leave a valid signed app on disk with a completion note. Confirm that the app actually launched on the device. The default minimum deployment target is visionOS 2.0, overridable with `IDTX_VISIONOS_DEPLOYMENT_TARGET` in the authoring environment.

Hand tracking is an explicit project opt-in. Test its permission prompt, actual recognized inputs, gaze/pointer interaction, transitions and re-entry after suspension. The WASM host uses interpretation on visionOS, so measure script cost on the physical device with realistic entity counts.

## Tune visual cost and record acceptance

Device `texture_quality` accepts `full_resolution`, `authoring` (2048-edge cap) or `inspector` (1024-edge cap), with mipmaps. Device targets default to `authoring`; desktop/PCVR use full-resolution runtime defaults. This changes runtime texture decoding, rather than performing an offline compression pipeline.

For each target, record project revision, package stamp, Conductor/runtime release, device/OS, boot success, interaction results, both transition directions, frame behavior and peak memory. Repeat checks after relocation/install. The [testing chapter](../testing.md) provides a checklist you can copy into your own experience's acceptance record.
