# Local HTML previews

An HTML file in a project on this device (`.html` or `.htm`) shows **Preview** in its Files editor header. Choosing it opens the saved file, rendered, in a Browser tab beside the conversation. The source editor stays open in its own tab. This covers generated reports such as a Playwright HTML report: a chat link to `playwright-report/index.html` opens the source in Files, and Preview shows the readable report without starting a server or finding the file in another app.

## Scope

The preview tab navigates only inside the HTML file's own folder. A report loads its own assets, subpages and hash routes. Links to sibling folders, parent folders, other files on disk, or a symlink inside the folder that resolves outside it are cancelled. A symlink that resolves outside the project is refused before anything loads. The tab can still open websites the report links to; `target=_blank` links to websites open in a new ordinary Browser tab.

- macOS loads the page with `loadFileURL:allowingReadAccessToURL:`, so WebKit's content process can read that folder only, whatever a page or symlink asks for. The navigation delegate cancels any `file:` navigation that resolves outside it.
- Linux passes the folder to the WebKitGTK helper, whose navigation policy and load command accept `file:` URIs only below it, after resolving symlinks. Images, scripts and stylesheets a page embeds are not filtered by folder there; WebKitGTK's default file-URL rules still stop a page's scripts from fetching other local files.
- Other platforms open the file in the system default browser.

Ordinary Browser tabs still accept only `http` and `https` addresses. Typing a `file:` address there explains that local files open from Files. The preview tab's address bar accepts addresses inside its folder and websites.

Choosing Preview again for the same file brings its tab forward and reloads the file from disk, which shows a regenerated report. The preview reads the saved file, not unsaved editor changes. Files from a project on another device show Preview as unavailable, because the file is not on this machine.

## Validation

- `scripts/cargo.sh test --locked -p luna-ui --lib browser::model` checks the folder scope: its own pages, query and fragment routes, sibling and parent folders, encoded `..` and separators, other hosts and schemes, symlink escapes from the project and from the folder, and that ordinary tabs refuse `file:` addresses.
- `scripts/cargo.sh test --locked -p luna-ui --lib rendered_preview_regressions` clicks the painted Preview control in a headless shell: it opens one folder-scoped Browser tab, Preview again reuses it, and a project on another device opens nothing.
- The native [browser fixture](../crates/ui/examples/browser-fixture/local_document.rs) runs on macOS and Linux CI. Starting from the document Preview hands to its tab, it checks that a relative asset and a subpage load, that history works, that a typed address inside the folder loads, that links (direct or through a symlink) and typed addresses outside the folder are refused, that opening the document again reloads it from disk, and that ordinary tabs keep refusing `file:` addresses. It saves a `browser-local-report.png` capture.
