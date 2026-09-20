# Remote Control operator steps

Desktop controller app: `personal-hopspot/remote-control-desktop/`
(Dioxus 0.7.5, Tokio `personal-rns` controller). The window title is
**PRNS Controller**. The top bar switches views: the nodes icon on the
left opens **Managed Nodes** (the default), the lightning icon beside it
opens **Flash** (desktop only), and the settings icon on the
right opens **Settings**. Flash lists the device catalog, writes this
checkout’s firmware, and enrolls UF2 and ESP boards as Managed Nodes for
this Operator so they are ready to manage without an invitation. MeshTower
V2 is in that catalog as an explicit pick: it shares the T114 UF2 Board-ID
(`HT-n5262`), so you choose MeshTower rather than letting auto-detect
guess.

The desktop app **embeds its own Personal Reticulum node**. It does not join
a `prnsd` shared instance on this machine, and it does not reuse that
daemon's routing tables or radios. Settings lists **this app's** local
transports (turn them on or off there). The interface list under a target
is **that device's** configured set. The two sides meet only when they
share a live medium — Auto Wi-Fi, TCP, BLE, or USB. That is two nodes
talking, not one process riding the other.

Only BLE Auto starts on. Set `HOPSPOT_RC_BLE=0` to boot it off; Start on
Settings turns it back on without restarting the app. USB, Auto Wi-Fi, and
TCP start off and stay attached so Settings can enable them. `HOPSPOT_RC_USB=1`
or `HOPSPOT_RC_AUTO_WIFI=1` starts those on. Settings lists a **TCP client**
card, default off, dialing `127.0.0.1:4242`. Configure on that card sets
this app's outbound `host:port` (IPv4 or DNS; default port 4242), stores it
in `tcp-target` under the controller data directory, and retargets the live
client. `HOPSPOT_RC_TCP=host:port` overrides that file at boot and starts
the client on. The Flash form's TCP field provisions a board, not this app.
If both this app and local
`prnsd` enable Auto Wi-Fi, they are independent peers on the LAN; turning
an interface off on Settings does not change the daemon. Do not run this
app's BLE and a `prnsd` Bluetooth Auto on the same Mac at once; one
process owns the radio.

Settings → Identity management shows this app's **Operator** hash and
allow-list key, and a separate **Instance** hash. Pairing inserts the
Operator hash into the target's controller allow-list. The 128-character
allow-list key is what another controller pastes when adding this app under
**Node Management Whitelist**. Instance never leaves this install; sibling
pins and roster sync use it.
**Sibling Controllers** lists other PRNS Controllers that share this
Operator, by alias plus Instance hash. Adopt can set the alias; otherwise
the dest app stores `Sibling 1`, `Sibling 2`, and so on.
**Configure** slides in USB adopt: stop BLE, Auto Wi-Fi, and TCP, leave exactly
one USB peer. The dest app presses **Allow a sibling to replace me** first.
The source app presses **Share Operator**, both check the six digits, both
Accept. Dest's Operator is replaced; dest's Instance stays. Quit and reopen
that app after adopt. Keep a copy of dest's `identities/controller` file first.
Both secrets are minted from the OS CSPRNG on first launch into
`~/.local/share/hopspot-remote-control/identities/controller` and
`identities/instance` (or `$HOPSPOT_RC_DATA_DIR/identities/…`) and reused
after that. Adoption copies the Operator secret, managed-node grants, sibling pins, and
roster labels (pairing names, target aliases, manager aliases, sibling aliases,
and non-USB peer aliases). After that, pair, forget, and later label edits
update the other app once both have heard each other's Instance roster announce
on a shared transport. Managed nodes start unmonitored. **Connect** on one
sibling claims that node for ten minutes: the other installs mark it Offline,
show Connect 00:00, and stop asking it for status. Another sibling can take
the node back by pressing Connect itself. The timer resets when Connect is
pressed and expires back to 00:00. A dest that already has sibling pins also pulls those
labels on launch without waiting to hear the other install. Pairing a target again after Forget records a newer upsert clock so a sibling's leftover tombstone does not delete the new grant. Approve waits for that grant to appear in inventory, writes its pairing name, and only then drops the awaiting row and pushes the roster snapshot. Entering the invitation code on one sibling dismisses that pairing advertisement on the others. An install's name for itself stays local ("This controller")
and is not copied; incoming sibling-alias rows for this Instance are ignored. Roster does not run during a USB sibling adopt.
Settings → Activity log records configuration changes made through this
app (Start/Stop, Save, pairing, Forget, Sleep/Wake, peer and target aliases).
Peer aliases are stored in `peer-aliases` under the controller data
directory and key on the full 8-byte interface id. Auto Wi-Fi peer rows
are titled by path (`UDP`, `TCP out`, `TCP in`) plus the socket address
when the Controller named that member (`To` / `From` / `Peer`). A `To` row
also shows `From (us)` with this device's address on that link; a `From` row
shows `To (us)` (Auto Wi-Fi inbound uses rendezvous port 42699). A UDP `Peer`
row shows `Local (us)` with this device's `fe80` on the same ifindex — the
address the other node lists as `Peer` for this path. The `P XXXX`
suffix is the first two hash bytes of the interface id and is used when
no address is available; it comes back the same after a reboot when the
peer identity is unchanged. UDP and TCP to the same node are separate
rows. Stock Auto Wi-Fi also dials the Wi-Fi gateway on port 42699; that
`TCP out` row is not a mesh node if it never connects. The Auto Wi-Fi
card title stays `auto-wifi` plus the supervisor id prefix; each IPv4 and
IPv6 the supervisor is using is a separate fact (`IPv6` link-locals include
the ifindex). `dummy0` is
treated as a virtual NIC and is not an Auto Wi-Fi peer. Target pairing names live in `target-names` (the announcement label)
from pairing). Optional aliases live in `target-aliases` beside that
name. Pairing does not fill the alias.

Pairing availability is a **one-shot hop-0 broadcast** when the target
opens its window. Ingress ignores any copy with a non-zero hop count, so
`prnsd` will not forward those ads. The desktop controller must already
be running on a shared transport (Auto Wi-Fi, TCP, BLE, or USB) so it can
hear that packet; starting the app after `pairing open` misses it.
Re-open pairing on the target if needed. MeshTower / T1000-E style boards
need a direct medium: leave BLE on (the default), wait for a BLE peer on
Settings, then Pair remote.

Heard announcements appear under **Managed Nodes** as “Awaiting pairing”. Expand one
to enter the invitation code and finish pairing there (there is no separate
Pairing screen). The firmware / PRNS version appears on its own line above
Address after the controller first reaches that target. Paired targets expand into a
list of interfaces; expand an interface to manage that feature.

## Pair with local prnsd

1. Start the daemon against a lab config with at least one live interface
   (`prnsd run --config …`).
2. Start the desktop controller on a transport that can reach the daemon, for
   example Auto Wi-Fi or TCP:

   ```console
   HOPSPOT_RC_AUTO_WIFI=1 \
     cargo run --manifest-path personal-hopspot/remote-control-desktop/Cargo.toml \
     --features desktop
   ```

   or, if the daemon exposes a TCP listener you can reach:

   ```console
   HOPSPOT_RC_TCP=127.0.0.1:<daemon-tcp-port> \
     cargo run --manifest-path personal-hopspot/remote-control-desktop/Cargo.toml \
     --features desktop
   ```

3. With the controller already up, open a pairing window and print the
   invitation:

   ```console
   prnsd pairing open --config …
   ```

4. On **Managed Nodes**, expand the awaiting entry (often labeled `prnsd`), enter the
   invitation code, and press **Continue**. The app should show a Connecting
   state, then the six confirmation digits. Compare those with:

   ```console
   prnsd pairing status --config …
   ```

   Those digits are derived from the pairing handshake (not the invitation).
   Both sides must match before you approve. If Continue times out, Pair
   remote again while this app is already running and use the newest
   awaiting row. Each Pair remote (and every flash) mints a new pairing
   destination; an older Hopspot line still shown after a failed Continue
   talks to a dest that is gone. A wrong invitation is also silent.
5. Approve on either side, in either order:

   ```console
   prnsd pairing approve --config …
   ```

   Press **Approve** in the desktop app when you are ready; it waits if
   the target has not approved yet. Both approvals must finish inside
   the attempt window (about two minutes after the confirmation digits
   appear). The open window itself is a bit longer (~three minutes) so
   you have time to enter the invitation code before Begin; those two
   timeouts must not be equal or Continue fails with an offer timeout.

   If Approve fails with `Timeout` / `AwaitingCompletion` after about
   thirteen seconds, rebuild the desktop app — older builds used the short
   link-default receipt timeout and could not wait for the target Yes.
   The same error after you already Yes'd the target first is a short
   attempt window on the target: Hopspot firmware used to open for 60s
   with a 30s attempt, and persist after that deadline rolls the grant
   back without sending Completed. Reflash so Pair remote uses the same
   ~3 min / ~2 min windows as `prnsd pairing open`.

   If Approve fails with `LinkClosed` on a Hopspot board, older firmware
   wrote the grant to flash before sending Completed; that flash stall
   can drop BLE and close the pairing link. Reflash so Completed goes
   out first and persist happens afterward.

   If Approve fails with `LinkClosed` / `NoActiveAttempt`, rebuild and
   restart `prnsd` — older daemons used `NoPersistence` for the node recipe
   and aborted the pairing link when applying the controller grant.
6. On **Managed Nodes**, expand the paired entry (it keeps the pairing
   announcement name; add an alias if you want a second label) and then any interface you want to manage. The list matches the
   daemon's configured interfaces (`prnsd status -a`), not individual peers.
   Use Connect / Announce / **Sleep** or **Wake** on the target — one
   button, **Wake** when the target is sleeping and **Sleep** otherwise —
   or **Start** / **Stop** under an interface title. **Configure** slides the status
   card aside and slides **Start** / **Stop** and **Configure** off so **Back**
   and **Save** take that same left-aligned row. You can then change mode
   (the seven `InterfaceMode` values the node already knows), on Auto
   Wi-Fi / LAN and Bluetooth Auto the discovery `group_id`, on a Hopspot
   Auto Wi-Fi card the station SSID and password (leave the password empty
   for an open network; saving replaces the stored password), and on a
   Hopspot LoRa or RNode card the region, center frequency, preset, SF,
   bandwidth, CR, TX power, and preamble. Changing region snaps frequency
   to that band's default and clamps TX to the regional ceiling. Settings
   Start/Stop and Configure match a target card. Those edits stay local
   until **Save**. **Back** returns to the status card when nothing has
   changed; **Cancel**
   does the same after edits and warns if anything is still unsaved. Closing
   the card, collapsing the target, or changing screens also warns. The Settings interface list and the
   Managed Nodes list refresh on their own about once a second. A target's
   interfaces, peers, and radio sample do not; the ↻ control on the
   list, a target, an interface, or a peer asks for a fresh inventory
   of that host. Supervisors come back on the inventory request; peers
   load in follow-up pages for each interface that has a fleet. Those
   icons still reload the same card. The controller asks for the
   supervisor list first, then each interface's config, then each
   fleet's peers, so one link packet never has to carry every fact.
   Expanding an interface shows the
   same card and menu facts Hopspot does: mode, connection (including
   Waiting / No Peers / Off), group, IFAC, gravity, TX/RX and their
   rates, links, carried links, destinations, combined rate, last
   activity, role or Config (LoRa also lists region, center frequency,
   preset, SF, bandwidth, CR, TX power, and preamble), Host/Port, drop
   counters, failure reason, and
   each supervisor peer's TX/RX, links, destinations, rate,
   activity, a `P XXXX` id with an optional alias, BLE Health (radio
   session vs a live RNS link — Connected can still be Radio only), and the radio sample that medium actually has (BLE RSSI,
   LoRa RSSI/SNR/quality; Auto Wi-Fi peers have no per-peer RF
   measurement).    After the interface list, **Node Management Whitelist** lists each
   manager address hash, an optional local alias, and Remove. A narrow
   window truncates the hash and alias; click the hash to read it in
   full. Configure adds a controller by pasting its Settings allow-list
   key. This controller cannot remove itself. Manager aliases live in
   `manager-aliases` under the controller data directory. On Hopspot the change is
   live immediately; flash persist of an edited list happens on the next
   pairing write, so a reboot before that can restore the previous list.
   Power, mode, group, LoRa tune, and Auto Wi-Fi station
   SSID/password are the remote actions the protocol already carries;
   Station / Reset / RNS Config stay on the Hopspot face. A live station
   apply lasts until reboot; the flashed provisioning slot is still what
   the board reads at boot. The board must already have a station stack
   (SSID flashed once) for the live apply to join. Below each
   target address are the last
   announce time in local time and a second line with hop count and inbound path. A stored
   PRNS build string and a **Battery** reading (percent, or USB when plugged with no
   cell) appear above the address when Connect/monitor is live and the board firmware
   exposes them; older firmware simply omits Battery. On MeshTower V2, percent comes from
   the VBAT ADC and external power from the HUSB238 USB-PD sink (20 V contract →
   charging; other attach → USB; SoftDevice 5 V alone is not treated as pack power).
   Controller-only updates cannot
   invent a reading — flash the board after this feature lands. A stored
   target starts Offline after this app restarts because reachability is not
   persisted; the node list itself is loaded from this install's persist and
   roster replica, then sibling roster deltas correct it. Nodes start
   unmonitored (Connect 00:00). A sibling that pressed Connect also leaves
   this install Offline for that node until this install Connects. Opening
   the app announces this controller's
   operator destination. Connect sends a path request to the target's stable
   remote-control destination, starts a ten-minute monitor, and drops the stored hop, then the
   interface list waits for a dest announce (or a path reply) before it
   opens a link. It also tells sibling Controllers to stop asking that node
   for status. Routing chooses USB or BLE. Expanding a target shows the
   last interface inventory from Connect; it does not fetch. The operator announce gives the target a
   path back to the controller that is in use, which a target probe cannot
   choose when several cloned controllers could answer. The target answers
   the path request if a shared transport is up. Announce is the opposite
   direction: it needs a live control link and asks the target to announce.
   The interface list waits for that control destination (not the one-shot
   pairing advertisement) only when no route is heard yet. A live hop is
   enough — the boot announce, the extra announce after
   target Approve, or a later one. Connect is the only way this install
   starts asking a node for status after a restart or after the ten-minute
   monitor expires.    Approving here first can send
   inventory in the same moment Completed arrives. Hopspot activates
   the controller grant in RAM before it writes flash, so that request
   should succeed without waiting for persist. The app still retries
   for about two minutes if the first inventory misses. Restarting
   `prnsd` reloads the pairing grant from
   `storage/prns/remote_control_controller_grants`. If that file is
   empty (an earlier empty flush), pair again. Without a grant the
   target hears inventory and declines it; the app retries that request
   until the grant appears or about two minutes pass.
   **Forget** removes that
   target from the controller's stored
   access list and local name file; pair again to bring it back.

## Flash a catalog board

On desktop, open **Flash**, select a catalog board to slide out its
flash info and options, then **Flash and enroll**. The form can write a
station SSID/password and an optional TCP client on ESP boards, and a
LoRa region/preset that is applied once the node is reachable. Leave
Wi-Fi blank to keep whatever is already in the provisioning slot.
hopspot-flash builds this checkout and writes a Remote Control identity
plus this Operator’s grant onto UF2 and ESP boards. The node then
appears under Managed Nodes — no invitation code. After a successful
enrolled flash, Flash offers to open that node. The T1000-E serial
DFU path flashes firmware only; pair that board after it boots. An
unprovisioned HV4 with no station SSID boots SoftAP so the LAN card is
on the face and in the node list.

## Pair with a Hopspot board

1. Flash ESP32 (HV4-R8) or nRF firmware that includes Remote Control
   (desktop **Flash**, including MeshTower, or `./tools/prns build hopspot`).
2. Start the desktop controller on a shared transport (BLE Auto starts
   on; enable USB, Auto Wi-Fi, or TCP from Settings, or set
   `HOPSPOT_RC_USB=1` / `HOPSPOT_RC_AUTO_WIFI=1` / `HOPSPOT_RC_TCP=…`). For MeshTower / T1000-E, wait until Settings shows a
   BLE peer before pairing.
3. Start the desktop app first so it can hear the hop-0 pairing ad. Then
   open pairing on the target:
   - OLED Hopspot (ESP32 / T-Echo): Global menu **Pair remote**.
     Note the eight hex digits on the screen now — a previous code will not work.
   - MeshTower V2 (no screen): long-press the user button. The status LED
     repeats the invitation: one long flash, a pause, then eight hex
     nibbles (a long flash is `0`; `1`–`F` are that many short flashes).
     A later long-press cancels the window. The awaiting row is labeled
     `MeshTower`.
4. On **Managed Nodes**, expand the newest awaiting Hopspot row (the label
   includes a short dest suffix), enter the code, and complete six-digit
   confirm. On a face, Approve / Reject on screen. On MeshTower, short-press
   approves and long-press rejects while the LED double-pulses. Either
   order is fine; both approvals plus the board writing the grant must
   finish inside about two minutes after the confirmation digits appear.
   Older Hopspot firmware used a 30s attempt window and fails Approve with
   `Timeout` / `AwaitingCompletion` if you Yes the board first and then
   walk back to the app.
5. Drive inventory, power, and mode from the expanded target and interface sections.
   MeshTower LoRa profile changes apply until reboot; that board has no second
   radio-profile flash page to persist them.

## Lab seed grants

For automated benches without pairing, seed controller grants as in
`personal-rns/examples/remote_control.rs`. Product path remains pairing with
empty grants at boot (`RemoteControlInitialControllerGrants::Nobody`).

MeshTower V2 can bake one Controller into the UF2 at build. Copy the 128-character
allow-list key from PRNS Controller Settings (not the 32-character Operator hash)
and build:

```console
HOPSPOT_RC_CONTROLLER_KEY=<128 hex characters> \
  ./tools/prns build hopspot mesh-tower-v2
```

That writes the grant the same way pairing would. It does not enroll the tower
on the phone: Managed Nodes still gets the control destination from pairing
(or another Controller-side import). A board that already persisted an empty
or different allow-list keeps the journal copy after reflash unless that
journal is erased. Pairing remains available when the key is omitted.
