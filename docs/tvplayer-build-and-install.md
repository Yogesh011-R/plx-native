# Build, install and run the TVPlayer `.ipk`

How to get an installable package of TVPlayer, put it on an LG TV and start it. For the project's
goal and status, see [tvplayer-plan.md](tvplayer-plan.md).

## What you get

| | |
|---|---|
| Package | `com.yogesh.tvplayer.debug_<version>_arm.ipk` |
| App id | `com.yogesh.tvplayer.debug` (the developer build) |
| Launcher tile | **TVPlayer debug**, with a DEV bar on the icon |
| Needs | an LG TV with **webOS 4.0 or newer**, and Developer Mode (no root needed) |

The debug build includes developer features (the on-screen frame counter and the `/tmp`
dev triggers). A release build (`com.yogesh.tvplayer`) needs `make FLAVOR=stable RELEASE=1 ipk`
and is not set up for this fork yet (plan milestone M8).

## 1. Build the `.ipk`

### Option A — GitHub Actions (works from any computer)

The webOS toolchain has no x86_64 Linux version, so this is the way to build on this PC.

1. Push to `main` on the fork:
   ```sh
   git -C ~/dev/tvplayer push
   ```
   The **CI** workflow starts. Its `cross-build (armv7) + artifact assertions` job builds the
   package in about 6 minutes.
2. Wait for the run and download the package:
   ```sh
   run=$(gh run list -R Yogesh011-R/plx-native -w CI -L1 --json databaseId -q '.[0].databaseId')
   gh run watch "$run" -R Yogesh011-R/plx-native --exit-status
   gh run download "$run" -R Yogesh011-R/plx-native -D ~/Downloads/tvplayer-ipk
   find ~/Downloads/tvplayer-ipk -name '*.ipk'
   ```
   Or open the run on GitHub (Actions → CI → the run) and download the `plxnative-<commit>`
   artifact under **Artifacts**; it is a zip that contains the `.ipk`.

Only pushes to `main` run CI. If the cross-build job fails, no package is uploaded.

### Option B — local build (macOS or arm64 Linux only)

```sh
make setup-env                                    # once: downloads the webOS NDK
rustup toolchain install nightly --component rust-src --component clippy
make ipk                                          # pkg/com.yogesh.tvplayer.debug_<version>_arm.ipk
```

CMake is also required. Full details: [building.md](building.md).

## 2. Prepare the TV (once)

1. On a computer, sign in at [LG's developer site](https://webostv.developer.lge.com/) (a free
   account).
2. On the TV, install the **Developer Mode** app from the LG Content Store, open it, sign in, and
   turn **Dev Mode Status** on. The TV restarts.
3. Open **Developer Mode** again and turn **Key Server** on. Note the TV's **IP address** and the
   **Passphrase** shown there.

The TV and the computer must be on the same network.

## 3. Install the `.ipk`

### With webOS Dev Manager (recommended)

1. Install [webOS Dev Manager](https://github.com/webosbrew/dev-manager-desktop/releases/latest)
   on the computer.
2. Add the TV: choose **Developer Mode**, then enter the TV's IP address and the Passphrase.
   Keep the defaults: user `prisoner`, port `9922`.
3. Open **Apps**, choose **Install**, and pick the `.ipk` from step 1.

### With the webOS CLI (`ares-*` tools)

If you have LG's webOS CLI installed:

```sh
ares-setup-device                                  # add the TV (Developer Mode, port 9922)
ares-install --device <tv-name> com.yogesh.tvplayer.debug_<version>_arm.ipk
```

## 4. Run it

- On the TV: open the launcher and select **TVPlayer debug**.
- From Dev Manager: **Apps → Installed → TVPlayer debug → Launch**.
- From the CLI: `ares-launch --device <tv-name> com.yogesh.tvplayer.debug`

The app opens on the **Videos** page (the file browser). Plug in a USB drive with video files.

Current limits (see the plan):
- **Playing a file does not work yet.** Choosing a video only logs the request (milestone M2).
- **USB location is unconfirmed.** The browser looks in `/tmp/usb`, `/media/usb` and `/mnt/usb`;
  if none exists you see "Connect a USB drive with videos" (milestone M3).

Remote: arrows to move, **OK** to open, **Back** to go up a folder (Back at the top folder leaves
the app). The Magic Remote pointer also selects rows.

## 5. Update or remove

- **Update:** install the newer `.ipk` the same way. It replaces the installed version.
- **Remove:** Dev Manager → Apps → Installed → TVPlayer debug → Remove, or
  `ares-install --device <tv-name> --remove com.yogesh.tvplayer.debug`.

## Keep Developer Mode alive

A Developer Mode session expires after a limited time. If it expires, the TV disables
Developer Mode on the next restart and **removes the apps installed through it**. Open the
Developer Mode app and select **EXTEND** before the time runs out. Key Server is only needed
when adding the TV to Dev Manager.

## Logs and troubleshooting

- The app writes its event log to `/tmp/com.yogesh.tvplayer.debug/plxnative-events.log` on the
  TV. Reading it needs shell access (a rooted TV), or the lab bridge described in
  [lab-diagnostics.md](lab-diagnostics.md).
- **The tile does nothing:** the TV is older than webOS 4.0.
- **Video does not play on some 2019 sets (k5lp/k3lp chips) installed through Developer Mode:**
  a known PlxNative limitation of LG's sandbox; see the README's "Known issues".
- **Rooted TV with SSH:** the faster developer loop is `make TV=<ip> install` once, then
  `make TV=<ip> deploy` / `make TV=<ip> run` (see [building.md](building.md)). It needs the
  local build (Option B).
