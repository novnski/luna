# Local HTML previews

An HTML file in a project on this device (`.html` or `.htm`) shows **Preview** in its Files editor header. Choosing it opens the saved file, rendered, in a Browser tab beside the conversation. The source editor stays open in its own tab. This covers generated reports such as a Playwright HTML report: a chat link to `playwright-report/index.html` opens the source in Files, and Preview shows the readable report without starting a server or finding the file in another app.

## Scope

The preview tab can read and navigate only inside the HTML file's own folder. A report loads its own assets, subpages and hash routes. Links to sibling folders, parent folders or other files on disk are cancelled. A symlink that resolves outside the project is refused before anything loads. The tab can still open websites the report links to; `target=_blank` links to websites open in a new ordinary Browser tab.

- macOS loads the page with `loadFileURL:allowingReadAccessToURL:`, so WebKit's content process gets read access to that folder only. The navigation delegate cancels any `file:` navigation outside it.
- Linux passes the folder to the WebKitGTK helper, whose navigation policy and load command accept `file:` URIs only below it.
- Other platforms open the file in the system default browser.

Ordinary Browser tabs still accept only `http` and `https` addresses. Typing a `file:` address there explains that local files open from Files. The preview tab's address bar accepts addresses inside its folder and websites.

Choosing Preview again for the same file brings its tab forward and reloads the file from disk, which shows a regenerated report. The preview reads the saved file, not unsaved editor changes. Files from a project on another device show Preview as unavailable, because the file is not on this machine.

## Validation

- `scripts/cargo.sh test --locked -p luna-ui --lib browser::model` checks the folder scope: its own pages, query and fragment routes, sibling and parent folders, encoded `..` and separators, other hosts and schemes, symlink escapes, and that ordinary tabs refuse `file:` addresses.
- The native [browser fixture](../crates/ui/examples/browser-fixture/local_document.rs) runs on macOS and Linux CI. It opens a generated report through Files' Preview action, checks that a relative asset and a subpage load, that history works, that links and typed addresses outside the folder are refused, that reopening reloads the same tab, and that ordinary tabs keep refusing `file:` addresses. It saves a `browser-local-report.png` capture.
