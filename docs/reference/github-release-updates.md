# GitHub release updates

LUNA's in-app updater reads `manifest.json` and `manifest.sig` from the
latest GitHub Release and downloads the app archive from that release's
version-specific URL. The manifest has a version and a SHA-256 digest plus
URL for every platform artifact. `manifest.sig` is a raw 64-byte Ed25519
signature over the **exact bytes** of `manifest.json`. The updater pins the
public key in [`dist/update-signing-public.hex`](../../dist/update-signing-public.hex).
GitHub hosts the files; the project's signing key authenticates them.
The app follows GitHub's latest published stable release; drafts and
prereleases are not offered through this channel.

The local **LUNA Alpha** channel is a separate frozen snapshot with manual
promotion and its own data. It cannot install updates through this feed.
See [app channels and the product-version policy](local-release-workflow.md).
Product `0.x` releases are published as GitHub prereleases; the local installer
does not create a public release.

## One-time setup

The signing key has already been generated for this project. Its private PEM
is at `~/.config/luna-release-signing/update-signing.pem` with mode `0600`.
Keep an offline backup: losing the key prevents this version of LUNA from
accepting future release manifests until a new app with a rotated public key
is distributed manually. Never commit or print the private PEM.

Set the GitHub Actions secret from that file once, using the repository owner's
authenticated `gh` session:

```sh
gh secret set LUNA_UPDATE_SIGNING_KEY --repo novnski/luna \
  < "$HOME/.config/luna-release-signing/update-signing.pem"
```

The release workflow refuses to publish if that secret is absent or if its
private key differs from the pinned public key. Manual `workflow_dispatch`
builds artifacts but does not publish a release and does not need the secret.

The repository and its Releases must be public for app users to fetch updates
without a GitHub login. If the repository remains private, the anonymous
GitHub download URLs used by the app cannot work.

## Publishing

1. Classify the change and use `python3 scripts/version.py bump patch` or
   `minor`, then update `CHANGELOG.md`. `python3 scripts/version.py check --metadata`
   validates the workspace, lockfile, and iOS marketing version.
2. Merge and verify the change on `origin/main`, then run
   `python3 scripts/version.py release-plan`. Only when publishing is explicitly
   requested, create and push a new annotated `v<version>` tag at that commit.
   Never replace a previously published version.
3. The release workflow builds Linux, macOS, and Windows artifacts. After all
   builds pass, it checks that their names match the tag, creates and signs the
   manifest, and publishes all assets in one GitHub Release.
4. Confirm the Release contains both `manifest.json` and `manifest.sig`, plus
   `luna-<version>-macos-arm64-app.tar.gz`. From the previous installed LUNA
   release, open Updates, download the new version, and verify restart.

The `luna-<version>-macos-arm64.dmg` remains the initial download. In-app
updates use the app tarball. Move `LUNA.app` from the DMG into Applications or
another writable folder before using in-app updates; a mounted DMG is read-only.
Apple Developer ID signing and notarization are
optional. They affect Gatekeeper's first-install experience but are separate
from update authentication. The release workflow may produce an ad-hoc-signed
macOS app when those Apple credentials are absent.
