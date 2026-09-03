# Running t-fit on a Raspberry Pi

The point of this: something has to be running for your phone to reach it. A
Pi costs a few watts and never sleeps, which is exactly the job.

Measured on the release build, so you know what you're signing up for:

| | |
|---|---|
| Binary | 5.1 MB, no runtime dependencies |
| Memory, idle | 7.5 MB RSS, 3 threads |
| Database | 156 KB for 1,636 entries |
| CPU per request | ~1.8 ms |
| CPU while idle | zero — the async runtime parks |

A Pi 5 will not notice this. It would run on a Pi Zero 2 W.

Substitute as you go:

- `<user>` — your login on the Pi
- `<pi>` — the Pi's hostname or IP
- `<pi>.<tailnet>.ts.net` — its MagicDNS name, if you use Tailscale.
  `tailscale status --self` prints it, and `tailscale serve status` prints
  the finished URL once step 6 is done.

---

## The one rule

**One server, one database.**

Today your Windows machine runs its own server against its own SQLite file.
Once the Pi is the server, Windows becomes a *client* — you open the Pi's URL
in a browser instead of launching `t-fit.exe`.

Run both and you get two databases that drift apart, with Withings sync
writing into whichever happens to be up. Pick one. The Pi.

---

## 1. Install Rust on the Pi

Raspberry Pi OS 64-bit, as your normal user:

```bash
sudo apt update && sudo apt install -y build-essential git
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

`build-essential` is needed because SQLite is compiled from source into the
binary. There is deliberately **no OpenSSL dependency** — t-fit uses rustls —
so there is nothing else to install.

## 2. Get the source onto the Pi

Two ways. Pick one.

### A. Via GitHub — if you want the repo anyway

Nothing has been pushed yet, so set it up on Windows first. The file bridge
can't run git against `E:\` (it isn't allowed to delete files, which git needs
for its temp objects), so run these in **PowerShell**:

```powershell
cd <your project folder>
Remove-Item -Recurse -Force .git -ErrorAction SilentlyContinue   # clear any half-made repo
git init -b main
git add -A
git status --short          # check nothing personal is staged
git commit -m "t-fit: local-first weight tracker"
```

`.gitignore` already keeps out `target/`, `*.sqlite3`, your `data/*.csv`
history and the `_*.tgz` transfer archives — so your weight data and Withings
credentials stay off GitHub. The `git status --short` line is there so you can
confirm that with your own eyes before the first commit.

Then create an empty repo on GitHub and:

```powershell
git remote add origin https://github.com/<you>/t-fit.git
git push -u origin main
```

On the Pi:

```bash
git clone https://github.com/<you>/t-fit.git ~/t-fit
cd ~/t-fit
cargo build --release
```

Updating later is then just `git pull`.

### B. Straight copy over Tailscale — no GitHub needed

The build directory is ~2 GB, so copy around it. `robocopy` excludes
directories by name at any depth, which is what makes this reliable:

```powershell
robocopy <your project folder> $env:TEMP\t-fit-src /E /XD target .git /XF "_*.tgz"
cd $env:TEMP
tar -czf t-fit-src.tgz t-fit-src
scp t-fit-src.tgz <user>@<pi>:/tmp/
```

(`robocopy` exits with code 1 on success — that means "files were copied",
not an error.)

On the Pi:

```bash
mkdir -p ~/t-fit
tar xzf /tmp/t-fit-src.tgz --strip-components=1 -C ~/t-fit
cd ~/t-fit
cargo build --release
```

If both machines are on a Tailscale tailnet, MagicDNS resolves the Pi's name
from anywhere — this works away from home as well as on the sofa.

## 3. Install the binary

First build takes a few minutes on a Pi 5. The binary lands at
`target/release/t-fit`. Put it somewhere stable:

```bash
sudo install -m 755 target/release/t-fit /usr/local/bin/t-fit
t-fit --version
```

## 4. Move your database across

This is the part that carries your history **and your Withings link** — the
refresh token lives in the same file, so sync keeps working the moment it
lands. You will not have to re-authorize.

On Windows:

Find it first — the app prints the exact path in **Settings (☰)**, or:

```powershell
(Invoke-RestMethod http://127.0.0.1:8787/api/version).db_path
```

It'll be this unless you passed `--db`:

```powershell
scp "$env:APPDATA\t-fit\t-fit\data\t-fit.sqlite3" <user>@<pi>:/tmp/
```

(Yes, `t-fit\t-fit\data` — the directories crate nests
`%APPDATA%\<org>\<app>\data`, and both are "t-fit".)

On the Pi:

```bash
sudo mkdir -p /var/lib/t-fit
sudo mv /tmp/t-fit.sqlite3 /var/lib/t-fit/
sudo chown -R $USER:$USER /var/lib/t-fit
```

Sanity check before going further. **t-fit is a server — it runs in the
foreground until you stop it**, so background it for the check rather than
expecting your prompt back:

```bash
t-fit --db /var/lib/t-fit/t-fit.sqlite3 --port 8787 --no-open &
sleep 2
curl -s localhost:8787/api/stats
kill %1
```

You should see your current weight, entry count and date range. If they match
what Windows was showing you, the database travelled intact.

(Running it without the `&` is fine too — it just sits there serving, and
you'd `Ctrl-C` it or use a second terminal for the `curl`.)

## 5. Run it as a service

`/etc/systemd/system/t-fit.service` — replace `pi` with your Pi login:

```ini
[Unit]
Description=t-fit weight tracker
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=pi
ExecStart=/usr/local/bin/t-fit --db /var/lib/t-fit/t-fit.sqlite3 --port 8787 --no-open
Restart=on-failure
RestartSec=5

# It only needs its own data directory.
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectControlGroups=true
ProtectKernelTunables=true
ReadWritePaths=/var/lib/t-fit

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now t-fit
systemctl status t-fit
journalctl -u t-fit -f        # follow the log; Ctrl-C stops watching, not the service
```

Note there is **no `--serve 0.0.0.0`**. It binds loopback only, and Tailscale
publishes it. That's the secure default — see the next step.

## 6. Publish it over Tailscale

t-fit has **no authentication**. Anyone who can reach the port can read your
weight history and change it. Tailscale solves this properly: only your own
devices can reach it, and you get real HTTPS.

```bash
tailscale status --self          # confirms the full name, <pi>.<tailnet>.ts.net
sudo tailscale serve --bg 8787
tailscale serve status
```

The last command prints the finished URL —
`https://<pi>.<tailnet>.ts.net/`. That exact string is what goes in
`--base-url` and in the Withings dashboard, so copy it rather than typing it.
It works from your phone anywhere in the world, not just at home, because
your phone is already on the tailnet.

Tell t-fit its public origin so the Withings redirect matches, by adding to
`ExecStart`:

```
--base-url https://<pi>.<tailnet>.ts.net
```

then `sudo systemctl daemon-reload && sudo systemctl restart t-fit`.

> **Never use `tailscale funnel` for this.** Funnel publishes to the open
> internet, and t-fit has no login. `serve` keeps it inside your tailnet,
> which is what you want.

If you'd rather also reach it over plain LAN, add `--serve 0.0.0.0` — but
then anyone on your home network can read and edit your data. Tailscale-only
is the better default.

## 7. Point Withings at the new address

Your existing link keeps working, because the token came across in the
database. You only need this for the *next* time you re-link:

In the Withings developer dashboard, change the callback URL to:

```
https://<pi>.<tailnet>.ts.net/api/withings/callback
```

This is a genuine HTTPS URL with a real certificate, which Withings is
happier with than `http://127.0.0.1`. The redirect happens in *your* browser,
which is on the tailnet, so Withings never needs to reach the address itself.

## 8. Switch Windows over to being a client

Stop the local one so it can't diverge:

- If you installed the Tauri app, uninstall it, or at least don't launch it.
- If you were running `t-fit.exe` from a terminal, stop doing that.

Then just bookmark `https://<pi>.<tailnet>.ts.net/`. In Edge or
Chrome, **⋯ → Apps → Install this site as an app** gives you a desktop icon
and a chromeless window — the same experience as before, pointed at the Pi.

Keep the old Windows database as a backup; don't delete it for a while.

## 9. Your phone

Open the Tailscale URL in Chrome or Safari and use **Add to Home Screen**.
You get the t-fit icon and a window with no browser chrome.

Because Tailscale gives you HTTPS on a real certificate, this is a proper
secure context — so offline support via a service worker is possible later
if you want it.

---

## Keeping it updated

```bash
cd ~/t-fit && git pull      # or repeat the copy from step 2B
cargo build --release
sudo install -m 755 target/release/t-fit /usr/local/bin/t-fit
sudo systemctl restart t-fit
```

The footer shows the version and build time, so you can confirm the restart
took effect.

## Backups

Everything is one file. A daily copy is a complete backup:

```bash
sudo crontab -e
# 0 3 * * * sqlite3 /var/lib/t-fit/t-fit.sqlite3 ".backup '/var/lib/t-fit/backup-$(date +\%u).sqlite3'"
```

That keeps a rolling week, one per weekday. Use `.backup` rather than `cp` —
it's safe to run while t-fit is writing.

## If something's wrong

```bash
systemctl status t-fit
journalctl -u t-fit --since "1 hour ago"
curl -s localhost:8787/api/version
curl -s localhost:8787/api/withings/status
```

`linked: true` means Withings is connected. `last_error` tells you what went
wrong most recently, and it survives restarts.
