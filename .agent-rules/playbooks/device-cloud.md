# Playbook: browser and device testing on BrowserStack (core D10)

Applies to every repository whose `PROFILE.md › Test matrix` lists a web or mobile surface. The profile owns the matrix;
this playbook says how a run is wired, recorded and kept honest.

## Credentials and accounts (owner-reserved, R8)
- Secrets `BROWSERSTACK_USERNAME` and `BROWSERSTACK_ACCESS_KEY` live at organization level (or repository level where the
  owner prefers), set by the owner. Agents never read, print, copy or move them; a `browserstack.yml` committed to the repo
  references them as `${BROWSERSTACK_USERNAME}` / `${BROWSERSTACK_ACCESS_KEY}`, never as literals.
- A run without the secrets (a fork PR on a public repo, a repo the owner has not enabled) records the matrix check as
  `unavailable: no BrowserStack secrets (<reason>)`, also when an `if:` guard skipped the job for that reason. a change that did not
  map to the surface (D2) reads `n/a: not selected (D2)`. Neither is ever reported as `passed`.
- Never use `pull_request_target` or `workflow_run` to give fork code the secrets, and pin `browserstack/github-actions/*`
  (and every other action) to a commit SHA (`playbooks/security.md`). A missing secret on a fork PR is the correct outcome.
- Plan, parallel count and device retention are owner decisions; the profile's Budgets section carries the ceiling.

## Web surfaces (Automate)
- Framework: the repo's existing runner (Playwright preferred; Cypress or Selenium acceptable) through the BrowserStack SDK
  (`browserstack-node-sdk` for Node: `npx browserstack-node-sdk <existing test command>`), configured by a committed
  `browserstack.yml` with `platforms:` entries of `os`, `osVersion`, `browserName`, `browserVersion`, a static `projectName`
  (the repo slug) and `buildName` `<repo>-<lane>-<sha7>`.
- Default matrix when the profile does not narrow it: Chrome latest and latest-1 on Windows 11; Edge latest and Firefox
  latest on Windows 11; Safari latest on the current macOS and latest-1 on the previous; Safari on the current iPhone and
  Chrome on a current Samsung Galaxy (real devices). PR fast lane: Chrome latest on Windows 11 plus any browser the change
  targets.
- The app under test must be reachable: deploy a preview, or start the dev server on the runner and open a tunnel with
  `browserstack/github-actions/setup-local` (close it in an `if: always()` step). Record which in the PR.

## Mobile surfaces (App Automate)
- The binary comes from CI, never from a laptop: Android `.apk`/`.aab` from Gradle on a Linux runner (Expo: `expo prebuild`
  then `./gradlew assembleRelease`); iOS `.ipa` from `xcodebuild -exportArchive` on a macOS runner (listed in the PROFILE `CI runners` row, D9) or
  from EAS Build when the repo uses it. Upload with the framework's own App Automate endpoint (`/app-automate/upload` for Appium; `/app-automate/maestro/v2/app`,
  `/app-automate/espresso/v2/app` and `/app-automate/xcuitest/v2/app`, each with its `/test-suite` counterpart) using a `custom_id` of
  `<repo>-<sha7>`; read the id with `jq -r .app_url` and print only that, with no `set -x`, `curl -v` or credential echo.
- Framework by stack: Expo / React Native → Maestro flows (or Detox, which App Automate runs for Android; confirm iOS support
  in the vendor docs before relying on it) on App Automate; native iOS → XCUITest; native Android
  → Espresso; anything else → Appium. Tests run against the uploaded build, on real devices.
- Default matrix when the profile does not narrow it: iOS current major and previous major on a current iPhone and on the
  oldest iPhone the app supports (plus one iPad when the app supports iPad); Android current major and the two previous on a
  Google Pixel and a Samsung Galaxy (one flagship, one A-series). PR fast lane: one current-OS device per platform running
  the smoke flows; iOS only where an `.ipa` exists for the commit (otherwise `unavailable: no iOS build on this lane`).

## Lanes and cost
- PR fast lane: smoke subset, one platform per surface, target under 10 minutes of device time per commit.
- Integration (push to main): the full matrix once per merge; batch if merges are frequent (testing-and-ci.md › Lanes).
- Release: full matrix plus the manual oracles the profile lists, evidence pasted with date, revision and operator.
- Parallels: use `parallelsPerPlatform: 1` unless the profile raises it; the owner's plan caps the total.

## Recording and anti-gaming (R5, R6, D7)
- The PR evidence table carries one row per surface: result, revision, BrowserStack build URL (public link disabled unless
  the owner says otherwise). A build with any failed session is `failed`.
- One automatic retry per failed session is allowed; a pass on retry is recorded as `passed (1 flake: <test>)` and the flake
  is listed under Limitations. Two flakes on the same test across PRs → open an issue; quarantining a test (skip, retry
  count above one, matrix row removed) is a High-tier change (R9) and needs the profile edited in the same PR.
- Never loosen a timeout or assertion to get a device green; name the device and OS in the finding and let the owner decide.
- Do not run device tests from a laptop against the cloud and paste the result as CI evidence; manual runs are recorded as
  `manual` with operator and date, and only on the lanes that allow manual oracles.

## Cleanup (R10)
- Uploaded apps: delete the `bs://` build in an `if: always()` step after the matrix finishes unless the profile keeps them
  for the release lane; BrowserStack also expires uploads after its retention period.
- Local tunnels: stopped in `if: always()`. Preview deployments: torn down by the lane that created them.
