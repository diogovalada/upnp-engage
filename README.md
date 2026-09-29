# UPnP Engage

Forward a TCP and UDP port through your router while the program is running. It shows the external address so you can copy it and send it to friends.

## Run it

Download the executable for your system from [Releases](https://github.com/diogovalada/upnp-engage/releases). Start the application or game you want to share, and make sure UPnP is enabled on your router.

- **Windows:** double-click the `.exe`.
- **Mac:** download the Apple Silicon (`arm64`) or Intel (`x86_64`) ZIP, extract it, and double-click `upnp-engage.command`. It opens in Terminal. If macOS blocks the first launch, go to **System Settings → Privacy & Security → Open Anyway**, then confirm **Open**. These builds are not Developer ID signed or notarized. If needed, drag the executable into Terminal and press Return.
- **Linux:** allow the downloaded file to run as a program in its file properties, then open it. If your desktop doesn't launch it, use a terminal:

  ```sh
  chmod +x upnp-engage-linux-x86_64
  ./upnp-engage-linux-x86_64
  ```

Windows and Linux releases target x86_64; Mac releases support Apple Silicon and Intel on macOS 14 or newer. Linux builds require glibc 2.35 or newer and an installed terminal for desktop launching. Each download contains one executable, with no companion launcher.

On the first run, enter your **device port** (the port your application uses), then the **router port** (the port friends connect to). Press Enter to use the same number. You can save the settings for next time or just run once.

Once forwarding is active, share the displayed `IP:port`. Press **C** to copy it, **P** to change ports, or **Q** to quit. Ctrl+C also quits. Keep the program open while friends are connected.

## Settings

A valid `config.toml` starts forwarding automatically:

```toml
device_port = 8080
router_port = 0
```

`router_port = 0`, or leaving it out, uses the device port. Set a different number when needed.

The program checks beside the executable, then the current working directory, then your user config directory. It loads the first file it finds. User locations are:

- Windows: `%APPDATA%\upnp-engage\config.toml`
- Linux: `$XDG_CONFIG_HOME/upnp-engage/config.toml`, normally `~/.config/upnp-engage/config.toml`
- Mac: `~/Library/Application Support/upnp-engage/config.toml`

Saving updates the loaded file. New portable setups save beside the executable; standard Unix install locations use the user directory. The save prompt shows the path. Changes stay temporary unless you save them.

## Command line

```sh
upnp-engage --device-port 8080 --router-port 9000
upnp-engage --config ./server.toml --device-port 8080 --save-config
```

Port options override saved values for that run. `--interactive` reviews settings, `--save-config` saves them, and `--non-interactive` disables prompts and terminal launching for scripts. Use `--help` for the full list.

## If something doesn't work

- **No router found:** check the network connection and the router's UPnP setting. A VPN may affect discovery.
- **Port already in use:** choose another router port. Existing forwarding rules are left alone. Routers must support checking individual mappings so ownership can be verified.
- **Friends can't connect:** the app displays the router's external address; it doesn't test internet reachability. Check your application, local firewall, and whether your ISP uses CGNAT or you have another router upstream.
- **Copy doesn't work:** select the address manually. Linux clipboard access depends on the desktop's X11/Wayland support. Keep the app open while pasting.

Normal shutdown removes the mappings, including when supported terminal-close notifications arrive. Force-killing the process, losing power, or losing contact with the router can prevent cleanup. Mappings request a one-hour lease, renewed while running, so routers that honor expiry remove them later.

To build from source, install Rust and run `cargo build --locked --release`.

Releases use tags such as `v0.1.0`. Update the crate version, lockfile, and changelog before pushing a tag. CI tests all four targets before publishing the downloads and SHA-256 checksums.
