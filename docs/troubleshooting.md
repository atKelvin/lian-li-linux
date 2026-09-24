# Installation troubleshooting

Open **Installation Health**, then **Recheck** after repairs. The page shows when file
checks last ran and distinguishes failures from checks it cannot verify. **Later** dismisses
the startup popup for this GUI session. No repair is performed automatically.

## Partially lit Galahad II fans

For Galahad II LCD and Vision (`0416:7391` and `0416:7395`), open **RGB → Fans →
LED count**, match the connected fan/ARGB chain and save. The supported range is
8–50, with a default of 24. The slider remains editable while Quick Sync controls
the effects. Counts stay saved across restarts and lighting preset changes.

The pump-head zone reports 12 logical LEDs for both models. This is separate from
the adjustable fan count and follows the vendor's lighting API, not a measurement
of physical LEDs. If the chain is still partly lit, include the exact fan models,
wiring and selected count in the issue report.

## Hermes permissions

Update the host's Lian Li Linux package to install the current `60-lianli.rules`.
For a source installation, run these commands **on the host**, from the project
directory:

```sh
sudo install -Dm644 packaging/udev/60-lianli.rules /usr/lib/udev/rules.d/60-lianli.rules
sudo udevadm control --reload-rules
```

Reboot when convenient, then recheck Installation Health. Installing the file only
inside Distrobox does not change host device permissions. An older copy in
`/etc/udev/rules.d/60-lianli.rules` overrides the packaged `/usr/lib` copy, so update
that local copy too if present.

The Hermes entries select unassigned host nodes dynamically. `renderD130` is not
a universal device number. They preserve root ownership and active-seat ACLs while
preventing later mode resets from disabling the ACL mask. Private-session and
explicitly assigned nodes are excluded. Do not add your user to the root group.

If access still fails, run `getfacl /dev/dri/renderD<N>` on the host for the exact
node reported by Health. A named user entry with `#effective:---` means its mask
denies access. Include that output, the installed Hermes udev rules and diagnostic
export in your report. Avoid blanket DRM permissions or changing private-session
nodes. See [desktop backend details](desktop-backends.md#hermes-kms-interface-baseline).

## Wired device identities after upgrading

Wired controllers and displays use their USB vendor ID, product ID, bus and port
path as their identity. USB serial strings remain metadata: some devices share
the same serial, including devices from different families. Wireless devices
continue to use their MAC addresses.

Existing fan, pump, RGB, LCD and ENE fan-quantity settings migrate when their old
identity has one unambiguous connected owner. Offline settings remain saved.
If multiple devices could own old settings, Installation Health reports the
ambiguity; configure each device using its USB port identity. Settings already
saved under a physical identity take precedence over legacy settings.

Keep wired devices connected to the same USB ports to retain those identities.
Changing ports or hubs can require selecting the device again. An unavailable
display is not automatically replaced with another connected display.

## GUI is offline

Fresh packages install both units but enable neither. Select one mode using
[service setup](service-modes.md). For Distrobox, manage the host user unit described in
the [Distrobox guide](distrobox.md), not a service inside the box.

For a native user daemon:

```sh
systemctl --user status lianli-daemon.service --no-pager
journalctl --user -u lianli-daemon.service -b -n 100 --no-pager
```

For a native system daemon:

```sh
systemctl status lianli-daemon-system.service --no-pager
journalctl -u lianli-daemon-system.service -b -n 100 --no-pager
```

Look for a missing ownership lock, permission errors, a configuration parse failure or
missing shared libraries. Do not delete a live daemon's socket or lock. The GUI and daemon
must be updated together; Settings shows the running daemon's version and configuration path.

If the GUI cannot load its desktop libraries, `lianli-control diagnose` provides
standalone read-only service diagnostics. It reports unavailable host bridges and
systemd queries without attempting to install helpers or start hardware control.

`lianli-control diagnose-runtime` checks the calling process's effective account,
numeric groups, shared host ownership lock and USB/HID node permissions. Run it as
your normal desktop user, without `sudo`, when diagnosing user-service setup. The
command defaults to the `hidraw` backend; use `--hid-backend rusb` if that is your
selected setting. Results are JSON findings; unavailable host integration is
reported explicitly. No ownership lock is acquired and no device I/O is performed.

When video/GIF media or a template containing video is selected, or a live H.264
encoder or H.264-to-JPEG fallback is reported, Installation Health also
checks `ffmpeg` and `ffprobe` in its own environment. Each tool has a one-second
deadline under the bounded runtime helper. The report lists whether software
`libx264` encoding is available; its absence is informational because JPEG playback
may not require it. These checks do not open a GPU or decode media. Install missing
tools inside the daemon's Distrobox when applicable, repair its service PATH, then
Recheck and save failed media settings to retry. Unselected templates do not enable
these checks. Media preparation/playback errors provide additional source-specific
failures; the tool inventory alone does not establish successful decoding or encoding.

The selected daemon also checks its state directory's effective write/search
permissions, mount writability, free bytes and available file entries. If the state
directory is absent, a writable existing ancestor is reported so a first save can
create it. A dangling path or non-directory is rejected. When media-tool checks
are relevant, the helper also checks the daemon's temporary-media directory;
that directory must already exist. These checks create no files, change no
permissions and run under the existing runtime-helper deadline. Quotas, security
policy and later filesystem changes can still make the actual operation fail.

Installation Health runs this check separately for the desktop process and,
through read-only IPC, the selected daemon. A desktop result cannot establish
access for the system daemon. An account database that contains the host lock's
GID while the process lacks it indicates stale group membership; log out/in and
restart the selected service. A still-running Distrobox also needs to be stopped
and entered again. Existing ACL access is recognized without requiring that group.

The GUI and daemon each need the matching `lianli-control` beside their executable.
Build the workspace for source runs and restart the rebuilt daemon. Each diagnostic
helper inherits its caller's UID/GID/groups and has a three-second execution timeout.
The caller waits at most 3.5 seconds. If helper cleanup is stuck in filesystem I/O,
its one worker slot remains occupied until cleanup finishes; Recheck cannot spawn
additional helpers. The daemon caches results for two seconds. Recheck does not
start hardware control or repair files. The daemon checks USB/HID node permissions
using its startup backend, even while a newly saved backend is awaiting restart.
Desktop credential checks do not guess the daemon's backend or verify its access.
See [USB permissions](usb-permissions.md) for missing nodes, custom rules and the
limits of metadata checks. Registry-driven wired device open failures appear as
**Device could not open**, with the latest error and device identity. They clear
after a successful open or discovery of removal. The report retains up to 64
devices; further failures require the daemon journal. Discovery retries are
limited, so reconnect the device or restart the selected service after repairing
the cause if retries have stopped. **LCD initialization failed** reports attach,
device initialization and initialization-worker start errors for up to 32 LCDs.
Successful initialization or removal clears the error. A late result from an old
attachment cannot clear the replacement's error. Recheck reads the last attempt
and does not initialize the device again. Desktop workers also report through
their stream status and daemon logs. Wireless discovery
reports connection and receiver-initialization failures in Health. If saved
wireless settings exist, an absent transmitter is reported too. A successful
initialization clears that finding; it does not verify every wireless endpoint
or guarantee fan/pump control after a later communication failure.

A **Changes disabled** banner means the GUI cannot verify a compatible daemon.
Install matching GUI and daemon versions, cleanly restart the selected daemon,
then refresh the GUI. Read-only inspection remains available where the older
protocol supports it. The daemon rejects writes from older clients lacking the
compatibility handshake; update the pixel-cleaner CLI alongside it as well.
Custom IPC clients must obtain `GetDaemonInfo` and wrap mutations in `Guarded`
with the matching client version, protocol version and daemon instance ID.
An interrupted response is not automatically retried: refresh and inspect the
applied state before repeating an operation that may already have succeeded.

For damaged configuration or template JSON, see [state backups](state-backups.md).

## Exporting a diagnostic report

Open **Installation Health → Diagnostic export** and choose **Preview export**.
The report uses the displayed check snapshot, daemon/GUI versions, service state
and media-preparation status. Recheck first when newer results are needed.

Ordinary LCD preparation results also show matching runtime source setup, H.264
source configuration or JPEG submission, its FPS limit and hardware-video policy,
and any live H.264-to-JPEG startup fallback. Old content retained while a
replacement prepares is not labelled as the new configuration. H.264 source
configuration and JPEG submission do not prove physical panel delivery; ordinary
H.264 streams report whether an HID access unit or WinUSB payload chunk has
successfully transferred, with a fresh flag for each file/live worker. An
unavailable status is distinct from waiting for the first transfer.
This first-transfer status does not measure continuous playback or confirm the
physical panel output. Selected encoder
metadata identifies the encoder that successfully transcoded a file or started a
live sensor/custom pipeline, including software fallback when hardware video was
allowed. Live metadata updates on encoder restart without waiting on frame writes.

The last sender playback error is retained through target removal and recovery,
until the next media preparation request. Errors from replaced media are ignored.
A live sensor/custom H.264 renderer that exhausts its retries is shown as stopped,
instead of continuing to report successful playback. It stays idle until media is
prepared again; save the media settings after fixing the logged cause to retry.
WinUSB asynchronous JPEG transfer failures also stop the affected source and
appear in the retained media error. Repair the connection and save the media
settings again to retry. A full sender queue is temporary contention: rejected
frames are not counted as submitted, and the latest frame remains eligible for
retry. Failures from a replaced media selection do not stop the new source.

WinUSB H.264 file/live transfer errors also stop the affected source and appear in
this history. Late errors from cancelled or replaced streams are discarded.
HID live-transfer worker exit also stops its encoder producer. HID file playback
keeps its existing per-transfer retries, then stops after an exhausted failure
until media is prepared again. Clean completion of a non-looping file is not a
failure and does not restart the stream.

For an OpenRGB SDK port conflict, free the configured port and choose **Retry
OpenRGB** in Settings. Retry uses saved settings and does not save other pending
edits. To change the port, edit it and Save instead. The queued message is not a
successful bind confirmation; check the resulting status. Enabled server failures
also appear in Installation Health. Reconfiguration closes and joins
the previous server's client threads before starting a replacement. Connections
are limited to 16 simultaneous clients and incoming packets to 1 MiB. Oversized
packets close that connection; excess clients are refused. These limits do not
change the normal device enumeration or per-LED command formats.

In **Template Browser**, use **Refresh** after a catalog or preview download
failure. Downloads have a 15-second deadline and a 1 MiB per-file limit; catalogs
support up to 128 entries. At most four previews download simultaneously and the
preview cache holds at most 16 MiB. Refresh or closing the window cancels pending
downloads and releases cached preview URLs. Unavailable previews do not prevent
installing a template. These browser limits are separate from daemon-side template
installation and its errors.

Daemon installation accepts at most 128 asset files, 64 MiB per asset, 1 MiB for
template JSON, and 256 MiB in total. Each download has a 10-second timeout.
The daemon's `templates` asset directory has an 8 GiB catalog limit, counting old
versions and incomplete installs. An install requires 256 MiB of quota headroom.
Before downloading, a read-only scan inspects at most 16,384 entries and 32 levels
of directories, checking a five-second deadline between filesystem operations.
Blocked filesystem operations themselves cannot be interrupted by that deadline.
Symlinks and special files require directory repair before installing. These
checks neither delete assets nor change existing playback; external copies into
the directory can still exceed the limit. This quota is separate from media
import storage used by service migration.
The download phase shares a two-minute deadline across all files and reuses one
HTTP client. Each install prepares a separate asset directory; ordinary download
or validation failures remove that partial directory. Verified assets are synced
before saving the template collection. Reinstalling preserves previous asset
directories so active playback and saved backups keep their original files.
An uncertain save failure retains the new assets as well; reload templates before
retrying. Process termination can leave an incomplete directory, and retained
directories currently require manual cleanup after checking template and backup
references.
Catalog file paths must be relative plain names without traversal, duplicate
destinations, or file/directory conflicts. Every template image, video and custom
font must be listed among the catalog's SHA-256-verified assets. Invalid paths,
unlisted assets, mismatched template IDs and unsupported daemon versions produce
an installation error.

The browser starts installation as a daemon operation and checks its status while
the window is open. **Check install** retrieves the latest operation after a
connection failure or reopening the browser. Only one catalog install runs at a
time, including requests from other windows or older clients. Closing the window
does not cancel installation. A lost connection is not proof of failure: check
status and reload templates before retrying. Status belongs to one daemon process;
after restarting it, reload templates to determine whether the save completed.
Daemon shutdown rejects new installations and cancels preparation before template
publication. An HTTP read already in progress can take up to its request timeout
to notice cancellation. Persistence that started before shutdown may finish its
atomic write. Hardware teardown does not wait for a stalled catalog filesystem;
process termination can therefore leave an unreferenced asset directory for later
cleanup.
New catalog directories contain a private `.lianli-catalog.json` ownership receipt,
synced before downloads start. It records directory identity and expected files;
its bytes count toward the installation quota. Older directories may lack this
record. The receipt is groundwork for managed cleanup, not proof that the assets
are unused: active settings, profiles and backups may still reference them.
New downloaded asset files use mode 0644 and completed directories use 0755;
the receipt remains private. These modes do not inherit group-write permissions
from the daemon's umask. Content-review support binds a directory's identity,
file metadata and SHA-256 contents into a review fingerprint, while reporting
missing or changed expected files. Unexpected files/directories, links, foreign
ownership or writable-by-others content fail review. Review is bounded to 256 MiB,
130 files, 32 directory levels and five-second checks between I/O operations.
This backend support is not yet a deletion action in the Settings card.
The read-only `GetCatalogStorage` IPC request lists directory sizes and receipt
ownership status, including legacy or malformed records. It does not establish
cleanup eligibility or delete files. Inspection is limited to 1,024 top-level
directories and the existing bounded tree scan; incomplete inspection returns an
error. Ownership validation checks directory identity, generated name, receipt
schema, file-path/hash structure and daemon-account ownership/permissions.
In **Settings → Catalog storage**, choose **Inspect storage** to view this report.

Older plain-name directories, such as `cooler`, remain usable but cannot be
automatically removed through this inventory. Their explanatory status refers to
cleanup verification, not playback or Unix account ownership. Do not copy a
receipt from another directory; receipts are bound to the original directory.
Filter directory names or saved-reference sources and expand a row for protection
details. Results show 20 directories per page and are cleared when the connected
daemon changes. Inspection runs only when requested; removing files requires a
separate content review and confirmation.
For a directory with verified ownership, choose **Review files** to inspect the
content fingerprint's file list, catalog matches, changed/partial assets, missing
files and current reference protection. The read-only review runs as one daemon
job; the dialog checks its cached result once per second while open. Closing the
dialog stops observation but does not cancel filesystem work. Reopening starts a
new review once the previous job finishes. A daemon restart or replaced review
requires another review. File contents and references are separate snapshots;
unchanged content can gain references, so deletion rechecks both.
The `StartCatalogRemoval` IPC request consumes a completed review ID and runs a
tracked removal. It reserves all daemon write slots and excludes new media
preparation while rechecking disk/runtime references and reviewed file contents.
`GetCatalogReview` reports `removed: true` only after removal and directory sync
succeed. A failed or interrupted request requires inspection and a new review;
it may have removed some files. For a review with no saved or runtime references,
confirm the permanent removal checkbox and choose **Remove reviewed directory**.
Changed assets are included in that removal. Closing the dialog stops polling;
use **Check removal result** on the storage card to resume observation. After
leaving Settings or restarting/switching the daemon, inspect storage and review
remaining files again. A lost connection or reported error never implies success.
Removal first saves one private `.catalog-removal.json` recovery marker in the
daemon state directory. If interruption leaves an empty directory after its
ownership receipt was removed, the matching marker permits another review and
confirmed removal. A replaced directory or unexpected files are preserved.
Finish a pending removal before removing a different generation. A stale marker
whose directory was already removed is replaced on the next confirmed removal.
Review refuses to start while media preparation or a template preview is active;
retry after it finishes. An admitted review pauses new media preparation while
existing playback continues. Waiting preparation remains cancellable, and a
template preview reports a retry error if admission takes three seconds.
Inventory also reports saved reference sources from configuration, template and
RGB-preset files, profiles, and their `.bak`/`.before-restore` copies. Up to 16
source names are returned per directory with the full source count. The scan
validates known schemas and checks raw JSON strings, including template children
and resolvable file aliases. Missing assets whose catalog directory is explicit
still protect that directory. Unresolved asset paths, corrupt/unreadable state,
or exceeded limits return an incomplete-inventory error. Limits are 16 MiB per
state file, 64 MiB total, 768 profile directory entries, 8,192 distinct strings and
five-second checks between operations. This saved-state snapshot does not cover
retired renderers, unsaved editor drafts, or concurrent state changes, so it still
does not establish deletion eligibility.
Runtime reference tracking separately retains supplied and resolved asset paths
observed before/after media preparation and template previews for the daemon
session. Old renderer and alias-target paths remain protected until restart.
Tracking is capped at 4,096 paths and 1 MiB of path text. An unresolved path or
capacity overflow makes runtime assessment incomplete for that session; repair
media paths and restart cleanly before attempting cleanup. Path resolution runs
outside daemon state and tracking locks. Concurrent preparation/state changes
still require revalidation before any future deletion; unsaved drafts that have
never been previewed are not tracked.

Desktop startup does not require opening the GUI in native packages: the
`lianli-session.service` user unit waits for the active graphical login and
discovers its compositor environment. Check `systemctl --user status
lianli-session.service` and `journalctl --user -u lianli-session.service` if the
secondary screen does not start. User masks/overrides can disable this startup.
Source builds and Distrobox require a compositor login command for the matching
helper; building binaries does not install startup integration. See
[Desktop display backends](desktop-backends.md) for setup and session requirements.

The **Desktop displays** card and export include each detected desktop panel's
USB attachment, capture backend, fallback reason, startup/pause/failure state and
last video policy delivered over USB. Successful transfer does not confirm the
physical panel displayed the frame. Hardware video being allowed does not prove
hardware encoding was selected. The encoder field reports the encoder that
produced the last delivered frame and whether it used GPU or CPU pixel input.
CPU readback and software-encoding fallback reasons are shown separately; the
daemon logs contain detailed errors. Failed
worker details remain visible during retry backoff and clear when a new attempt
starts. Older daemons do not provide this structured desktop status.

After repairing a desktop prerequisite, choose **Retry desktop display** on the
failed panel in Installation Health. The request resets that panel's bounded
retry budget after its previous worker finishes cleanup. Capture waits for a
ready graphical session. Healthy displays are not restarted, and installing an
optional backend does not automatically move a working display to it. Recheck
refreshes diagnostics; Retry requests capture recovery. Older daemons without
the retry capability require an update before this button is available.

Daemon logs are required. The connected daemon's user/system journal is selected
automatically, with a manual service selector when needed. The report requests up
to 100 messages from the current service invocation, or the latest journaled
invocation when the service has stopped. Older daemon runs are excluded, even
within the same system boot. Colored journal messages are decoded and terminal
color codes removed before redaction. Only journals readable by the current desktop
account are included. Unavailable access is reported without requesting privileges.
Distrobox uses the checked host bridge.

For a manually launched daemon, use **Choose saved daemon log** and select a file
containing its terminal output (at most 1 MiB). The last 100 nonempty lines are
included and redacted. The source filename is not exported. If logs are absent,
unreadable or entirely omitted by redaction, the preview explains the problem and
saving is blocked; a report without daemon logs cannot be exported.

If your desktop account cannot read the running system daemon's journal, save an
extract from a terminal and select it in the export dialog:

```sh
umask 077
daemon_invocation=$(systemctl show lianli-daemon-system.service --property=InvocationID --value)
sudo journalctl --unit=lianli-daemon-system.service "_SYSTEMD_INVOCATION_ID=$daemon_invocation" --no-pager > daemon.log
```

For a user service, obtain its ID with `systemctl --user show lianli-daemon.service
--property=InvocationID --value` and use `journalctl --user --unit=lianli-daemon.service`
with that invocation filter, without `sudo`. Run host-service journal commands on
the host when using Distrobox.

Exports exclude configuration contents, environment variables, container names,
socket/config paths and capture authentication fields. Free-text details containing
paths, common asset filenames, credential terms or long opaque identifiers are
omitted conservatively. Other personal text can remain, so review the preview
before sharing. Selected logs may provide backend/fallback evidence; unavailable
runtime details are not presented as verified.

**Save JSON** opens a local file dialog and saves the exact reviewed snapshot as a
private file. Nothing is uploaded. Reports are limited to 128 KiB; journal queries
are bounded and only one preview can run at a time. A blocked query retains its
worker slot until cleanup finishes, preventing repeated attempts from accumulating
processes. Closing the dialog without choosing a file cancels saving.

## Devices or LCD media are missing

For TL and SL-INF Flex receivers, and wireless SL, TL, TL Flex and SL-INF Flex
fan LCD groups, the daemon automatically attempts recovery when fewer LCD USB
devices appear than the receiver reports. This includes completely missing groups.
Recovery requires an unambiguous USB hub mapping; wireless groups also need their
USB companion's MAC address to match a bound receiver.

The existing USB scan runs every ten seconds. After a ten-second discovery grace,
the daemon stops playback on that group, sends its LCD reboot command and allows
six seconds for rediscovery. A group gets at most two attempts while its receiver
remains connected, at least thirty seconds apart. Other groups keep playing.
If the group remains incomplete, check USB and power connections and power-cycle
it. Recovery does not reset the USB hub or change wireless binding.

**Hardware owner unreachable** means a process holds the hardware lock but the
GUI cannot reach daemon IPC. Wait for startup or a service switch to finish, then
Recheck. If it persists, inspect daemon logs and runtime socket visibility.
A daemon in Distrobox may listen in a private runtime directory. See
[testing a box daemon with a host GUI](distrobox.md#test-a-box-daemon-with-a-host-gui).
This finding alone does not mean `lianli-control` is missing.

Follow [USB permissions](usb-permissions.md) for device access. For media, `ffmpeg` and
`ffprobe` must be available in the **daemon's** environment, with software H.264 encoding
through `libx264`. Fedora requires full FFmpeg from RPM Fusion instead of `ffmpeg-free`.
Files and every parent directory must be accessible to the daemon account, including
child images, video and fonts referenced by custom templates. A file accessible to your
GUI user can still be inaccessible to the system daemon's `lianli` account.

See [LCD asset access](lcd-assets.md) for direct and template-child checks, system-mode
storage and preparation error recovery.

[Hardware video](hardware-video.md) is optional and disabled by default. Start with software
video when investigating GPU failures. Desktop mode additionally requires a usable display
backend and graphical session. Ordinary fan/RGB control does not require an EVDI kernel module.
See [optional kernel modules](desktop-backends.md#optional-kernel-modules) for host
kernel updates, missing headers, build failures and signing errors.

## Missing temperature readings

Software-controlled fans and pumps switch to 100% after five seconds without a valid
temperature, immediately if no valid reading has arrived. They resume their curves when
readings recover. Constant speeds, device-managed control and motherboard sync retain
their own behavior. The daemon logs fallback and recovery transitions. Installation
Health shows **Cooling fallback active** while a temperature source or curve is unavailable.
Recheck after repairing it. The status describes requested duty, not measured fan/pump speed.

Custom temperature commands run in the background. Each command must finish within one
second, return a finite numeric value first, and produce at most 8 KiB of output. Keep
commands under 16 KiB and use no more than 256 active commands per cooling controller.
Slow or failed commands cannot block fan/pump calculations. Unused command subscriptions
expire after 15 seconds.

H2 cannot safely refresh its coolant reading during H.264 LCD playback. A coolant-based
software curve therefore uses the fallback once that cached reading expires. Choose a
live CPU/GPU sensor or stop playback to restore fresh coolant-based control.

## Scope of the current checks

Installation Health checks host USB rules, service selection and ownership, optional
display modules, and the desktop user's runtime prerequisites. The connected daemon
reports its own environment, effective USB-node permissions, media tools and saved-state
errors. GUI and daemon environments are identified separately, including mixed native
and Distrobox launches. Unreachable checks remain unverified.

Permission checks use the daemon's credentials and visible filesystem without opening
USB devices. They do not prove that a driver can open a device or that every security
policy allows access. Service findings cover visible units and owners, not every other
user's private service setup.

LCD preparation checks direct and template-child assets when applying media. Installation
Health shows preparation and desktop-stream failures. Successful permission, module or
encoder checks do not prove correct panel playback. Include daemon logs when reporting
a failure and review the diagnostic preview for private information before sharing.
