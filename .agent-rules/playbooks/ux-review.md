# Playbook: product/UX review (trigger: anything a person sees changed; D3)

When: before coding for a new flow (acceptance criteria + sketch); after build for implemented behavior.
Input: PR number, classification, `PROFILE.md › Review triggers › ux` (surfaces, sizes, themes, how to run/screenshot),
brand/design docs named there.
Evidence required: rendered captures of each changed screen/state at the listed sizes and themes; keyboard, error,
empty and loading states; the acceptance criteria checked off.

1. Build the changed surface as `AGENTS.md › Commands` says, in your own worktree. If CI already uploads screenshots or
   golden images for the head SHA, use those first (D1) and capture only what is missing. Common captures when the
   profile names none: web — `"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new
   --screenshot=<file> --window-size=390,844 <url>` (repeat at desktop width, light and dark); iOS — `xcrun simctl io
   booted screenshot <file>`, `xcrun simctl ui booted appearance dark|light`; Android — `adb exec-out screencap -p >
   <file>`; macOS window — `screencapture -l <windowid> <file>`; rendered output — the repo's render-to-file or golden tests.
2. Walk the real flow end to end with test data. Never real accounts or cards.
3. Check: layout/spacing/type consistency, contrast (WCAG AA text, 3:1 UI), truncation/clipping, light and dark, platform
   conventions; immediate feedback and no needless spinners; one primary action per screen, safe back/cancel; offline /
   slow / empty / huge / malformed data / interrupted; screen-reader labels and order, tap targets ≥44 pt (24 px desktop),
   large text, reduced motion, every supported language fits.
4. Findings as in code-review.md, each with the state (theme, size, language, condition) and the capture file.
   Polish items are *suggestions* unless the profile names the surface as primary; they never become unbounded blocking work.
5. If something cannot be run (no simulator, Windows-only), record `unavailable` with what would be needed.
