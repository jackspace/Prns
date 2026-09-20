# Personal Hopspot Remote Control Desktop

A desktop-first Dioxus client for operating paired Personal Hopspot
remote-control targets. The window is titled **PRNS Controller**. Use the
nodes icon (left), the lightning **Flash** icon beside it (desktop only),
and the settings icon (right) in the top bar to switch
between **Managed Nodes** (the default), **Flash**, and **Settings**.
Flash presents the Hopspot device catalog. Select a board, open **Configure
and flash**, pick a firmware image from the dropdown (published tips and local
catalog entries), use **Import** / **Build** / **Export** under the list as
needed, set options, then **Flash**.
UF2 and ESP boards are enrolled onto Managed Nodes for this Operator. The
T1000-E serial DFU path flashes firmware but still needs pairing.

The image dropdown lists the current published **stable** tip, then the
**preview** tip (same label whether or not already downloaded; selecting either
fetches via `hopspot-flash fetch --channel …` when needed), plus other images
already under `~/.reticulum/controller/images`. Flash uses `--candidate` for
published tip images and `--developer-artifacts` for **Import** / **Build**
entries.
**Import** unpacks a shared one-board package (`.zip` with `target.json` +
firmware at the zip root or in one top-level folder, or an already-unzipped
folder). **Export** writes the current catalog image as that same zip shape for
sharing. **Build** (checkout only; otherwise the button is disabled) runs
`hopspot-flash build`. Catalog badges show
`Published · {channel} · v{version}`, `Imported · v{version} (unsigned)`, or
`Local build · {gitsha12} · v{version} (unsigned)`. The Flash tab runs
`hopspot-flash check` on open and every 30 minutes so the tip version stays
current.
Overrides:

- `HOPSPOT_FLASH` — absolute path to a `hopspot-flash` binary (preferred)
- Sidecar next to the Controller executable (`hopspot-flash` / `hopspot-flash.exe`),
  or `Contents/Resources/hopspot-flash` inside a packaged macOS `.app`
- `hopspot-flash` on `PATH`
- Checkout cargo fallback for fetch/check/build/flash when no binary is found
- `PRNS_CONTROLLER_IMAGES` — catalog root (default `~/.reticulum/controller/images`)
- `PRNS_CONTROLLER_HOME` — parent of `images/` when the images override is unset
- `PRNS_CONTROLLER_FLASH_LOCAL_BUILD=1` — emergency escape hatch; compile and flash
  from this checkout at Flash time instead of using the catalog

Portable packages also embed tree-built board firmware under `firmware/` (next to
the exe, or `Contents/Resources/firmware` on macOS). On first catalog access the
Controller seeds those boards into `~/.reticulum/controller/images` as **Bundled**
images and selects them when the board has no other current image. Published CDN
releases are skipped for day-to-day Flash until they include remote-control
support that matches this app.

Bundled board set today: `heltec-v4` (HV4 R2), `heltec-v4-r8` (HV4 R8),
`mesh-tower-v2`. (Heltec V3 is not in the flash catalog yet.)

From a checkout, build the flasher once and point the Controller at it:

```console
cargo build -p hopspot-flash --release
export HOPSPOT_FLASH="$PWD/target/release/hopspot-flash"
# or: export PATH="$PWD/target/release:$PATH"
```

CLI users can inspect the published channel without downloading firmware:

```console
hopspot-flash check --channel stable --board heltec-v4-r8 --json
```

## Portable packages (unsigned)

All three platforms produce **unsigned** archives. Flash still works without
`HOPSPOT_FLASH` when the packager places `hopspot-flash` where the resolver
looks (see layout below). Install dioxus-cli 0.7.5 first:

```console
cargo install dioxus-cli --version 0.7.5 --locked
```

### macOS `.app`

Embeds `hopspot-flash` and `firmware/` at `Contents/Resources/`:

```console
./tools/prns run release.controller.macos.package
# → target/controller-macos/PRNS Controller.app
# → target/controller-macos/PRNS-Controller-macos-<arch>-unsigned.zip
#    <arch> is aarch64 (Apple Silicon) or x86_64 (Intel), matching the host.
```

Optional flags (after `--`):

```console
./tools/prns run release.controller.macos.package -- \
  --hopspot-flash /path/to/hopspot-flash \
  --out-dir /tmp/controller-macos
```

Gatekeeper may block first launch; remove quarantine or right-click → Open when
testing:

```console
xattr -dr com.apple.quarantine "target/controller-macos/PRNS Controller.app"
open "target/controller-macos/PRNS Controller.app"
```

The packager injects macOS privacy usage strings (`NSBluetoothAlwaysUsageDescription`,
local-network / Bonjour) into `Info.plist`. Without those, Finder launch aborts
immediately under TCC with no UI error.

CI: `controller-macos-package` (`workflow_dispatch`) builds **both** `aarch64`
(`macos-14`) and `x86_64` (`macos-15-intel`). Developer ID codesign + notarization
happen in the PRNS-Controller release shell.

### Linux portable

Folder layout: `PRNS-Controller/<exe>` with `hopspot-flash` and `firmware/` beside it:

```console
./tools/prns run release.controller.linux.package
# → target/controller-linux/PRNS-Controller/
# → target/controller-linux/PRNS-Controller-linux-<arch>-unsigned.tar.gz
```

CI: `controller-linux-package` (`workflow_dispatch`) builds **both** `x86_64`
(`ubuntu-latest`) and `aarch64` (`ubuntu-24.04-arm`). Board firmware is staged once
on x86_64 and passed in via `--firmware-from`.

### Windows portable

Folder layout: `PRNS-Controller/<exe>.exe` with `hopspot-flash.exe` and `firmware/`
beside it:

```console
./tools/prns run release.controller.windows.package
# → target/controller-windows/PRNS-Controller/
# → target/controller-windows/PRNS-Controller-windows-<arch>-unsigned.zip
```

CI: `controller-windows-package` (`workflow_dispatch`) builds **both** `x86_64`
(`windows-latest`) and `aarch64` (`windows-11-arm`). MSI/NSIS installers are out of
scope for this slice.


## Prerequisites

Install the Dioxus CLI at the same version as the application:

```console
cargo install dioxus-cli --version 0.7.5 --locked
```

## Run

From this directory, use either:

```console
dx serve --desktop
```

or:

```console
cargo run --features desktop
```

## Android (arm64)

The same crate builds as **PRNS Controller** for Android via Dioxus mobile.
BLE Auto, USB Auto, TCP, and Auto Wi-Fi attach there the same way they do
on desktop. BLE starts on; USB, Auto Wi-Fi, and TCP start off. Grant
Bluetooth permissions when the app asks. USB uses the Hopspot JNI host
(WebUSB `1209:0001` and Android accessory). Pair over BLE, USB, LAN, or a
TCP path to `prnsd`. Do not run this app's BLE and Hopspot's BLE on the
same phone at once.

```console
export JAVA_HOME="/Library/Java/JavaVirtualMachines/temurin-17.jdk/Contents/Home"
export ANDROID_HOME="/opt/homebrew/share/android-commandlinetools"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
export NDK_HOME="$ANDROID_NDK_HOME"
dx build --android --release --target aarch64-linux-android \
  --no-default-features --features mobile
adb install -r target/dx/personal-hopspot-remote-control-desktop/release/android/app/app/build/outputs/apk/debug/app-debug.apk
```

Application id: `org.personal.prns.controller`. Identities persist under
the app files directory (`…/files/hopspot-remote-control`).

## Configure the controller

The application always starts its own Tokio `personal-rns` controller node.
It does not attach to a running `prnsd` shared instance. Settings shows this
node's local interfaces; target sections show the remote device's
interfaces. The two communicate only over a shared transport (Auto Wi-Fi, TCP, BLE,
or USB), as two separate nodes:

```console
HOPSPOT_RC_TCP=127.0.0.1:4242 cargo run --features desktop
```

Only BLE Auto starts on. Set `HOPSPOT_RC_BLE=0` to boot it off; Settings
Start/Stop turns each local transport up or down without restarting. USB,
Auto Wi-Fi, and TCP start off. Set `HOPSPOT_RC_USB=1` or
`HOPSPOT_RC_AUTO_WIFI=1` to start those on. Settings lists a **TCP client**
card, default off, aiming at `127.0.0.1:4242`. Configure on that card sets
this app's outbound `host:port` (IPv4 or DNS; default port 4242) and keeps
it in `tcp-target` under the data directory, then retargets the live client
so Android does not need an env var or restart. `HOPSPOT_RC_TCP=host:port`
overrides that file at boot and starts the client on. The Flash form's TCP
field provisions a board, not this app. Do not run this app's BLE and a local `prnsd` Bluetooth Auto at
the same time. Pairing ads stay hop-0; `prnsd` will not forward them.
Stable Operator and Instance identities, the host BLE identity, and paired-target
access are stored under `~/.local/share/hopspot-remote-control`. Override
that location with `HOPSPOT_RC_DATA_DIR`. Settings shows the Operator hash
   that pairing writes onto the target (`identities/controller`) and this
   install's Instance hash (`identities/instance`). Sibling Controllers
   share Operator only. The BLE identity is `identities/ble` in that
   directory. The controller TCP client target lives in `tcp-target` there.
   Peer aliases live in `peer-aliases` there, keyed by the
   full interface id (the `P XXXX` label is a durable short prefix of that id).
   Target pairing names live in `target-names` (the announcement label from
   pairing). Optional target aliases live in `target-aliases` beside that
   name. Pairing does not fill the alias; type one if you want a second
   label. Sibling aliases live in `sibling-aliases`, keyed by Instance hash.
   Adopt can set the name; otherwise the app stores `Sibling 1`, `Sibling 2`,
   and so on. Settings → Sibling Controllers lists adopted installs by that
   alias. Configure slides in USB adopt: dest arms **Allow a sibling to
   replace me**, source presses **Share Operator**, both confirm the six
   digits.    Dest's Operator is replaced; Instance stays. Quit and reopen the
   dest app after adopt. The adopt snapshot includes pairing names, target
   aliases, manager aliases, sibling aliases for other Instances, and
   non-USB peer aliases. Later roster sync uses Instance and also pulls
   those labels from pinned siblings on launch without waiting to hear
   them. **Connect** claims that node for this install for ten minutes;
   siblings mark it Offline, show Connect 00:00, and stop asking it for
   status. This install's own sibling-alias row is local-only.

## Pair a target

Pairing availability is broadcast once when the target opens its window. Start
the controller first so it is already listening. Heard announcements show up on
**Managed Nodes** as “Awaiting pairing”.

1. Start the desktop controller on a transport shared with the target
   (BLE Auto is on by default; enable USB, Auto Wi-Fi, or TCP on Settings, or set
   `HOPSPOT_RC_USB=1` / `HOPSPOT_RC_AUTO_WIFI=1` / `HOPSPOT_RC_TCP=…`). For MeshTower /
   T1000-E, wait for a BLE peer on Settings, then Pair remote.
2. Then open pairing on the target: `prnsd pairing open`, **Pair remote** on
   a Hopspot face, or a long-press on MeshTower V2 (the status LED blinks
   the eight hex nibbles; short-press later approves the six digits). Note
   the invitation code.
3. On **Managed Nodes**, expand the awaiting entry and enter that code, then
   **Continue**.
4. Compare the six confirmation digits on both devices (`prnsd pairing status`
   prints them). Approve on either side, in either order. The desktop
   waits if the target has not finished yet.
5. Expand the new paired target to see its interfaces. The firmware / PRNS
   version appears on its own line above Address after the controller first
   reaches that node. The row keeps the pairing announcement name; type an optional alias
   beside it. The list uses any
   heard route to the target's remote-control destination (the pairing
   advertisement does not install that route). If this app Approves
   first, that first inventory can miss until the target Approves; the
   app retries for about two minutes. The interface list is the
   same configured set `prnsd status -a` shows, not each discovered peer.
   The list, each interface's config, and each fleet's peers are separate
   requests so one packet never has to carry every fact.
   After the interface list, **Node Management Whitelist** lists each
   manager address hash and its alias as read-only text. A narrow window
   truncates those text fields; click a hash to read it in full. Configure
   turns the alias into an edit field, adds Remove, and adds a controller
   with the 128-character allow-list key from Settings.
   Expand an interface for the same card and menu facts Hopspot shows
   (mode, connection, group, IFAC, traffic, rates, links, destinations,
   activity, role/host, drops, per-peer traffic/link/rate/activity, and
   the radio sample that medium owns)
   plus **Start** / **Stop** and **Configure**. Each peer shows a durable
   `P XXXX` id and an optional alias you can type beside it. LAN / Auto Wi-Fi and
   Bluetooth Auto cards also show the discovery group id. Settings
   Start/Stop and Configure match a target card. Configure slides the status card
   aside and slides **Start** / **Stop** and **Configure** off so **Back** and
   **Save** take that same left-aligned row. Mode, group, and LoRa tune
   edits stay local until    **Save**. A Hopspot Auto Wi-Fi card can set the station SSID and
   password (empty password is an open network; saving replaces the stored
   password). A LoRa or RNode card can change region,
   frequency, preset, SF, bandwidth, CR, TX power, and preamble. **Back**
   returns to the status card when nothing has changed; **Cancel**
   does the same after edits and warns if anything is still unsaved.
   Settings and the target list
   refresh about once a second;
   ↻ on a target, interface, or peer reloads that host's inventory.
   Below the target
   address are
   two lines: last announce time in local time, then hop count and inbound path.
   Stored targets come back Offline after a restart from this install's
   persist and roster replica; sibling roster deltas then correct the list.
   The app announces this
   controller's operator destination and probes each target with a path
   request. Nodes start unmonitored (**Connect 00:00**). **Connect**
   drops the stored hop so routing can hear a fresh dest announce, starts
   a ten-minute monitor, and tells sibling Controllers to show
   Connect 00:00 and stop asking that node for status. Pressing Connect
   again resets the timer. The interface list then waits for that announce.
   Expanding a target shows the last interface inventory from Connect;
   it does not fetch. **Forget**
   drops the stored pairing from this controller.

If you opened pairing before starting the app, open it again (or close and
re-open) so a fresh advertisement is heard. Configuration changes made
through this app are listed under Settings → Activity log.
