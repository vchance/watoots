# Playbook: device testing on simulators and emulators (core D10)
Applies when the repository has a mobile surface (native iOS, native Android, Expo / React Native, Flutter). Web surfaces keep
their in-repo end-to-end suite and browser engine; nothing here adds a browser matrix or a device cloud (owner decision 2026-11-02).

## Targets (profile-owned)
- `PROFILE.md › Device testing` names, per surface: the test runner (XCUITest, Espresso, Maestro, Detox, Flutter integration
  tests), the PR smoke target (one simulator or emulator: e.g. `iPhone 17 / iOS 26` on `macos-15`, `Pixel 8 / API 36`), the full set
  (current OS plus the oldest the app supports, one small and one large screen where the UI differs), and where it runs.
- Default full set when the profile is silent: iOS current major and the minimum deployment target on the current iPhone and
  on an iPad if supported; Android current API and the `minSdk` API on a Pixel-class AVD.

## iOS Simulator
- Build and test with `xcodebuild test -scheme <S> -destination 'platform=iOS Simulator,name=<device>,OS=<version>'`; for Expo,
  `npx expo run:ios --device "<name>"` then the runner (Maestro: `maestro --device <udid> test flows/`).
- The hosted label must ship an Xcode with that simulator runtime (`xcrun simctl list runtimes`): pick the runner image from
  the OS version you need, not the reverse (`macos-15` images do not carry the iOS 27 runtime; that needs the `macos-26` image).
- Pick the simulator by `xcrun simctl list devices available -j`; create one only if the name/OS pair is missing (record the
  udid) and delete it at the end (`xcrun simctl delete <udid>`); shut down what the run booted (`xcrun simctl shutdown <udid>`).
  Never `xcrun simctl erase`, `delete` or `shutdown` a simulator the run did not create or boot (by udid, never `all`), and
  never `erase all` (R10).
- CI: a macOS job on the `MACOS` runner class (D9: `if: ${{ vars[format('RUNS_ON_MACOS__{0}', github.triggering_actor)] != '' && (<existing>) }}`
  and `runs-on: ${{ fromJSON(vars[format('RUNS_ON_MACOS__{0}', github.triggering_actor)] || '["unplaced"]') }}`; nobody has a Mac runner
  until an operator places the class, so the job is skipped and the row reads `unavailable: no macOS runner`), `timeout-minutes` set, derived data cached per Xcode version. Hosted macOS minutes cost about ten times Linux: run on PR only when D2 maps the change to
  the iOS surface, full set on main and release.
- No macOS runner available: run on the owner's Mac and record `manual (simulator, operator <owner|agent>, <date>, <sha7>,
  <device/OS>)` from a clean working tree at the PR head; the row carries the test command's exit code (D8).

## Android Emulator
- Create an AVD from a system image the profile names (`sdkmanager "system-images;android-<api>;google_apis;x86_64"`, from
  the default Google repository only, which verifies checksums; on the owner's Mac the agent does not run `sdkmanager
  --licenses`, the owner accepts once; in CI use the image's accepted licenses or the pinned action),
  `avdmanager create avd -n ci-<api>-<run id or pid> -k … --device pixel` (never reuse an existing name, never `--force`), boot headless (`emulator -avd ci-<api>-<run id or pid> -port <port> -no-window -no-audio
  -gpu swiftshader_indirect -no-snapshot`), wait for `sys.boot_completed`, run `./gradlew connectedCheck` (Espresso) or the
  Maestro/Detox flows, then stop only the emulator the run started (`adb -s emulator-<port> emu kill`, never `pkill emulator` or `adb kill-server`)
  and delete the AVD the run created.
- CI needs hardware acceleration: GitHub-hosted Ubuntu runners expose `/dev/kvm` once the udev rule step from the
  android-emulator-runner README grants the runner user access; check the machines an operator placed `MEDIUM` on once (`test -w /dev/kvm` after that
  step) before relying on it and record `unavailable: no KVM on runner (<class>, <operator>)` if absent. A pinned
  `reactivecircus/android-emulator-runner` action (by commit SHA, `playbooks/security.md`) wraps the above.
- Lane: PR smoke on one emulator when D2 maps the change to the Android surface; full set on main and release.

## Evidence and anti-gaming (R5)
- Each row of the PR evidence table names the target and the result: `passed (iPhone 17 / iOS 27, 42 tests)`, `failed`,
  `manual (simulator, …)`, `unavailable: no macOS runner` / `unavailable: no KVM on runner (<label>)`, `n/a: not selected (D2)`,
  `n/a: no mobile surface`. A skipped job is never read as a pass.
- One automatic retry per target, recorded as a flake with the failing test name; never loosen a timeout or assertion to get a
  target green; removing or downgrading a target is a profile change (High tier, R9).
- Screenshots and videos the runner produces are written to the artifact store (D11: `$CI_ARTIFACT_STORE/<org>/<repo>/<run_id>-<run_attempt>/<name>/`
  on the build box, retention 14 days) behind the run-time guard in `testing-and-ci.md` § Artifacts and caches, and the path, exit code and a one-line summary are in the row; they are not uploaded to GitHub, and a runner with no store fails the step.
  Flows use dedicated test accounts and fixtures, never real credentials; logcat and device logs are not uploaded unless the
  profile asks; in a public repository artifacts are downloadable by anyone with read access.

## Cleanup (R10)
- Simulators and AVDs created by the run: deleted in an `if: always()` step (CI) or a `trap … EXIT` (scripts). Emulator and
  simulator processes: killed. Derived data and Gradle caches: left to the runner's cache action, never committed.
