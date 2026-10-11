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

The macOS 27 SDK exposes a `State` macro whose plugin is absent
from the installed Command Line Tools. `NativeContentView` uses
the `ViewState` alias to select SwiftUI's property wrapper instead.
The normal release build should work without an SDK override.

```sh
./make-release.sh
```
