# iPad trackpad scrolling regression

Use a physical iPad and Magic Keyboard. Linux tests and SDK-free Apple type checks cannot
exercise UIKit's indirect input or establish that this bug is fixed.

Build and reinstall with `npm run ios:build` (or `-- --without-web-browser` if required by
signing). Confirm it prints `Trackpad input: native indirect input enabled in the built app.`
The flag is an Info.plist setting: hot-reloading JavaScript does not update it.
Also confirm `Window orientation: single-window Tao scene declared in the built app.`
The first candidate (indirect input alone, commit `9cb5dd3`) did not fix the issue on the owner's
iPad. The next candidate adopts Tao's single-window scene lifecycle; it still needs device testing.

1. Launch in landscape without rotating first. Open a long page and navigate to another page
   so Back is available. With the pointer over content, scroll up/down using two fingers.
   The page should move vertically, without navigating Back or Forward.
2. Open Settings and scroll its long sections with the trackpad. This must also scroll vertically.
3. Repeat both checks in portrait, then rotate back to landscape and repeat. Test both landscape
   orientations, and relaunch in each. The direction must stay correct.
4. Repeat with Swipe between pages and Pull to refresh on, then off. Vertical scrolling should
   work in each configuration. Check intentional horizontal history swipes with Swipe enabled.
5. Check a horizontally scrollable page element: horizontal gestures should scroll horizontally.
   Verify touchscreen scrolling and pull to refresh still work, and repeat content scrolling in
   split view and at non-default page scales.
6. Check scene lifecycle regressions: cold launch in each orientation, background/resume,
   keyboard shortcuts after resume, and opening an external http/https link into Snorkle.
   Check resizing with Stage Manager if used. There should still be only one app window.

Record iPadOS version, device, build commit, launch orientation and results. In a debug build,
Menu → Console records `scroll probe` lines. Capture ordinary up/down and left/right gestures
separately and label the physical direction and orientation; a translated pan alone does not
identify which direction the fingers moved. The previous report was portrait working, landscape
broken, and physical left/right gestures logged as vertical pans with no held modifier keys.
Include the new `scroll orientation` and `scroll window vector` lines. `scene true` confirms the
live window has a scene; interface orientation 3/4 identifies the landscape direction. Comparing
the window vector with the original view vector helps identify where a rotation occurs.
