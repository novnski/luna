# Luna for iOS

The current-source comparison and pending migration live in
[Native iOS parity](../../docs/pending-tasks/ios-parity/README.md). The guide
below describes the existing edge-backed client and direct paired-Mac setup.
Paired Macs verifies identity, enrolls and reconnects read-only access. After
authentication it opens one pinned SDK socket and shows `Live · host …` once
the engine's typed identity read names the paired Mac. Remote task controls,
projections and reconnect remain later migration work (RM-03 onward); pairing
alone does not certify provider execution.

A native SwiftUI viewport onto the luna mesh. The phone is a **peer
device**: it joins the same Loro CRDT rooms as every other device (workspace
doc + per-chat session docs over the edge's Durable Objects), renders the
mirrors, and drives remote engines through the durable command queue. No
engine runs on the phone.

## Build & run

Requires Xcode 26+ (iOS 26 SDK — Liquid Glass APIs).
Install Rust and the `aarch64-apple-ios` target for physical devices, or
`aarch64-apple-ios-sim` (`x86_64-apple-ios` on Intel) for simulators. The Xcode
build phase runs `scripts/ios/build-mobile-core.sh` through the repository
Cargo wrapper with `--locked`: Debug builds the core with Cargo's dev profile
and Release with `--release`; each architecture in `ARCHS` gets its Rust
target (combined with `lipo`), and the iOS 26 floor is enforced. It links the
shared native core, regenerates its Swift bindings from that same library and
records `target/ios-core/<platform>/identity.json` (targets, profile,
toolchain, revision and library/binding hashes). CI fails when the committed
`Luna/Generated/luna_core.swift` differs from what the core generates.

The core (`crates/mobile`) speaks the public SDK (`crates/sdk`) to a paired
Mac over a socket that `Luna/Remote/PinnedSocketTransport.swift` owns; see
[IOS-00](../../docs/pending-tasks/ios-parity/ios-00/README.md) for its
contract.

```sh
cd apps/ios
xcodebuild -project Luna.xcodeproj -scheme Luna \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

Or open `Luna.xcodeproj` in Xcode and run. Dependencies (SPM, resolved
automatically): [loro-swift 1.13.x](https://github.com/loro-dev/loro-swift)
(matches the engine's loro 1.13), [swift-markdown](https://github.com/swiftlang/swift-markdown)
(cmark-gfm: tables/strikethrough/tasklists — the same feature set as the
desktop's pulldown-cmark config).

The app has no hosted sync endpoint by default. For a Luna-owned service,
provide `LUNA_EDGE_URL` and `LUNA_WORKOS_CLIENT_ID` in the app bundle's
`Info.plist`. Without both values, sign-in explains that sync is unconfigured.
Demo mode works without either value.

The app icon is the Icon Composer document `Luna/AppIcon.icon`, which the
target's `AppIcon` setting picks up from the synchronized `Luna` folder. Its
SVG layers are byte-identical copies of the macOS sources in
`dist/macos/icons/` ([app icons](../../docs/reference/app-icons.md)): Porcelain
for light, Monolith for dark. Tinted and clear modes show the silver moon
without the background. After changing the macOS SVGs, copy them into
`Luna/AppIcon.icon/Assets/` (`0_Moon.svg` → `Moon-<Light|Dark>.svg`,
`background.svg` → `Background-<Light|Dark>.svg`). Then open the document in
Icon Composer, or preview a rendition with Icon Composer's `ictool`:
`ictool Luna/AppIcon.icon --export-image --output-file out.png --platform iOS
--rendition Dark --width 1024 --height 1024 --scale 1`.

## TestFlight release

Run the **TestFlight** workflow from GitHub Actions on `main`. It compiles the
iOS app and tests, selects the next numeric build number from App Store
Connect, archives with automatic signing, uploads an internal-only TestFlight
build, and waits for Apple processing to report `VALID`.

The workflow requires an App Store Connect app registered as
`com.novnski.luna.ios`, plus the `AC_API_KEY_P8`, `AC_API_KEY_ID`, and
`AC_API_ISSUER_ID` repository secrets.

### Connecting

- **Paired Mac**: no cloud sign-in is required. Open **Connect to a Mac** from
  sign-in or **Paired Macs** from the account menu. On the Mac, enable
  **Settings → Connections → Control this Mac**, choose its LAN address, and
  select **Add**. Scan or paste the one-use code, review the Mac and read scopes,
  then confirm. The phone pins the Mac's certificate and verifies its exact
  profile before spending the code. A saved connection reauthenticates on
  foreground; the Mac must be awake and reachable on that route.
  Tokens use app-owned, device-only iOS secure storage without biometric or
  password prompts. Preferences contain only host metadata. **Forget** removes
  this phone's saved connection; **Revoke access** on the Mac ends the grant.

- **WorkOS**: configure the owned edge URL and client ID in the bundle, then
  sign in (`/auth/exchange` and `/auth/refresh` use that edge).
- **Dev**: against an `AUTH_MODE=dev` edge (e.g. `wrangler dev`), enter a user
  id + org id; the bearer is `userId@orgId`.
- **Demo mode**: fully offline dataset with a scripted streaming reply —
  explore the UI with no infrastructure. Launch args for screenshot rigs:
  `-demo [-route chat:<id>|space:<id>] [-stream]`.

### Project sessions

Home's **+** menu creates sessions in an existing project or opens **New
space…** when the account has no projects. The **All** list shows sessions from
live projects; archived sessions retain their project association. A project
filter scopes the list to that project.

Run the project-session UI regression on an Xcode 26+ Mac:

```sh
cd apps/ios
xcodebuild -project Luna.xcodeproj -scheme Luna \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -only-testing:LunaUITests/ProjectSessionUITests test
```

Demo fixtures accept `-no-projects` (empty spaces/chats), `-ios-only` (no
execution hosts), and `-sethomefilter <spaceId>` (an empty value selects All).

### Full UI suite (local only)

CI runs only `LunaTests`. The 14 `LunaUITests` (project sessions, appshots,
mobile polish) run locally, because macOS simulator minutes cost about ten
times Linux ones (CORE-02 decision). Run the whole suite and keep its result
bundle:

```sh
cd apps/ios
xcodebuild -project Luna.xcodeproj -scheme Luna \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -resultBundlePath ../../target/qa/ios-ui-tests/$(date -u +%Y%m%dT%H%M%SZ).xcresult \
  -only-testing:LunaUITests CODE_SIGNING_ALLOWED=NO test
```

## Architecture

```
Sync/
  LoroProtocol.swift    loro-protocol 0.3 wire codec (byte-compatible port of
                        the crate's encoding.rs: magic/varBytes/type/payload)
  RoomClient.swift      room.rs port: join with oplog VV, snapshot backfill,
                        resubmit-from-server-VV, DocUpdate+Ack, fragments,
                        %EPH presence sub-room, ping/pong lease, backoff
  WorkspaceStore.swift  ws3/{org}/{user} mirror: devices/spaces/chats/sessions
                        rows, presence heartbeats, viewer-side writes
                        (createChat, archive, lastSeenAt, own device row)
  SessionStore.swift    session doc mirror: entries/parts (continuations
                        joined), command ledger appends (rule 1), host nudge
Markdown/
  MarkdownModel.swift   block model + incremental tail re-parser (re-parse
                        from the 2nd-to-last top-level block; link-defs force
                        full parses) — parser.rs port
  Highlight.swift       line tokenizer with carry state, paint-only
  MarkdownBlockView.swift  mobile metrics: body 17/26, headings 23/31…17/26,
                        code 14/21 (Dynamic Type scaled line rows), violet inline code,
                        accent blockquotes, hairline tables
Transcript/
  TranscriptRows.swift  rows_for_entry port: block-granularity rows, stable
                        ids ({msg}#{part}.{block}, {msg}#g{n}), fingerprint
                        versions, consecutive-tool grouping
  NativeTranscriptTable.swift  UIKit row reuse, gesture anchoring and
                        animated local-send runway retained across navigation
  TranscriptView.swift  SwiftUI message content, user-message folding, follow
                        (70pt re-engage, 140pt jump), tool activity rail
  Veil.swift            paint-only streaming fade (EMA-tracked duration,
                        1−(1−p)^1.6 curve)
Composer/               glass pill, Send→Steer→Stop morph, QuestionPanel
                        (paged, numbered options, 220ms auto-advance)
Theme/                  theme.rs port: oklch→sRGB converter, exact palette,
                        Geist/Geist Mono, motion timings + flavour words
```

### Parity notes (desktop ⇄ mobile translations)

| Desktop | iOS |
| --- | --- |
| Sidebar: Spaces + attention-sorted Sessions | Home screen sections (same sort ranks: awaiting > errored > working > completed > idle) |
| Horizontal session tabs per space | Space detail: vertical session list (creation order) |
| Tab close = archive | Swipe-to-archive |
| Archived shelf under the sidebar list (open by default, Show-more paging, hover-swap Unarchive) | Same shelf under Home/space lists; unarchive is swipe-to-unarchive |
| Status word in the row corner (muted dots; Done keeps its pop; spinner rides bottom-right) | Same, same colors |
| Composer `white_alpha(0.03)` pill + hairline | Liquid Glass pill (`glassEffect`) + hairline |
| Harness brand SVG marks (icons.rs) | Same path data via a native SVG path parser (`BrandMarks.swift`) |
| Harness/model picker popover + curated catalogs | Brand-mark cards + catalog menu + reasoning-ladder chips (`HarnessCatalog.swift`, ported from crates/harness) |
| Add-space palette (device + folder browser) | New-space sheet: device tabs + remote folder browser (ListFolders over the device-room relay, git repos badged) |
| ControlRpc over device-room relay | `DeviceRelayClient` — binary `uleb128(len)+header+payload` frames, `{"s","k","to","from"}` header, ndjson ControlRpc; used for ListFolders + direct-to-host `Mutate {createSpace}` (local doc-write fallback when the host is offline) |
| Hover timestamps / copy | Context menus |
| Long user messages: Show more / Show less | Five-line preview, 44pt disclosure target, expansion retained per session |
| gpui `list()` sum-tree virtualization | Native table row reuse with SwiftUI content, stable row ids and version fingerprints |
| Stick-to-bottom spring, wheel-up breaks pin | Gesture-owned follow, composer-sized viewport with glass underlap, retained local prompt runway |

Status colors, font families, veil timing, command-ledger shapes, and wire
protocol follow desktop. Text sizes and touch targets are adapted for phones;
see [mobile polish and simulator coverage](../../docs/mobile-polish.md).

### Writer discipline (what the phone writes)

- Workspace registry: chat creates (host = the project's owning device),
  `archived`/`title`/`lastSeenAt`
  LWW sets, presence heartbeats. The phone owns no engine device row.
- Session docs: append commands (`run`/`steer`/`interrupt`/`respondInput`)
  with client-minted message ids for optimistic echo; add and reorder shared
  queue rows. Queue edits and removals require host acknowledgement. The host
  writes all transcript entries and command outcomes.
- After queuing a command it POSTs `/device/{host}/nudge` so a cold host
  opens the doc and drains — delivery stays durable in the doc regardless.

### Shared message queue

Messages submitted during an active turn use the host's shared queue when it
advertises support. The composer toolbar's **Queue / Steer** menu persists the
phone's preference; **Queue** remains the default. Steer allows automatic
mid-turn delivery when the provider supports it. Attachments wait for a turn
that can accept files.

Rows offer **Steer** for a host-advertised mid-turn provider and text-only
messages, or **Send now** (interrupt) otherwise. Unknown provider capabilities
leave delivery actions unavailable. Edits require a host lease; removal waits
for the host's acknowledgement and keeps the row inert while pending. Failed
or unconfirmed actions show an error above the composer and trigger sync repair.
Deleting an actively edited row discards it without first releasing its lease.

With an external keyboard, **Command+Return** submits the draft (including
attachment-only drafts), saves an active queue edit, or activates the first
queued row when the composer is empty. It never skips a blocked head or stops
the agent merely because the draft is empty.

Queue editing on iOS changes text only and preserves queued attachments, including when the text is cleared. Draft photos are hidden and the attachment picker is unavailable during editing. If the row disappears or its lease is superseded, **Copy edit and stop editing** saves the edited text to the clipboard and restores the original draft and photos.
