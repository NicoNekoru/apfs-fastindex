# Browser interaction checks

Use a temporary fixture with nested folders, an empty folder,
mixed-case extensions, a symlink and more than 400 siblings.
Do not test Trash on personal files.

- Right-click a folder, file, search result and treemap cell.
  Each offers Reveal in Finder, Copy Path and Move to Trash.
  The scan root has Trash disabled. Cancel a Trash confirmation
  and verify the file still exists.
- Click an extension. The tree lists exact category matches in
  the current subtree, the selected category is marked, and other
  treemap leaves dim. A `.jpg.backup` file must not match `.jpg`.
- Narrow the category with a name query, switch the size metric,
  and clear with Escape. Selecting a result clears the filters,
  highlights the row and shows its containing folder.
- Use Show more in a large folder and in filtered results.
  Select a search result outside the first folder page and verify
  it remains visible in the hierarchy.
- Hide both panels, press Cmd-F and type. The tree must reappear
  with search focused. Empty results and empty folders explain
  why nothing is displayed.
- Navigate using the tree, treemap, breadcrumb and Cmd-Up.
  File selection must not turn the current subtree into a leaf.
- Rescan while a search is pending, including a fixture with the
  same entry count. No old node indices or filter results should
  survive. A failed rescan retains the prior scan and shows the
  error. Repeated Return must not start overlapping scans.
- With a scanned window, an empty window and Settings open, Find
  must be enabled only in the scanned window. Administrator Scan
  must be disabled in Settings and for an empty path. Neither
  command may dispatch to a background window. Opening another
  browser window must not rename Settings.
- Choose Don't Ask Again, then re-enable the reminder in Settings.
  The next scan must show it again. Open System Settings from both
  the reminder and Settings. Neither action should start a scan.
  Do not grant permissions during this check.
- Before launching a separate test bundle, save an update interval
  of Int.max in its own defaults domain. It must launch without
  overflowing and Settings must clamp the value to 168 hours.
- The pre-scan card shows volume capacity, not a guessed file count.
- Worker counts apply to both standard and administrator scans,
  including repeated requests using the same helper session.
  Helper protocol tests run without elevation and cover omitted,
  automatic, serial and parallel worker counts.

The macOS 27 SDK exposes a `State` macro whose plugin is absent
from the installed Command Line Tools. `NativeContentView` uses
the `ViewState` alias to select SwiftUI's property wrapper instead.
The release build selects the installed macOS SDK with `xcrun`
while keeping the deployment target at macOS 13. SDK 27's default
Swift Build engine incorrectly records that deployment target as
the build SDK. The script uses SwiftPM's native engine until that
is fixed and rejects executables whose SDK metadata does not match
the selected SDK. This prevents compatibility styling changes from
silently reaching a release.

```sh
./make-release.sh
```
