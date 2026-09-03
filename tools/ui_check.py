"""Live UI check for per-profile Withings registration.

Drives the real app in a real browser against a scratch database. Two things
it has to prove, because unit tests can't: that saving one profile's client
id doesn't reach across into another's, and that the "Registration details"
disclosure stays where the user put it while a link attempt is polling in
the background.

Run against a server already started on --port with an empty --db.
"""
import argparse
import re
import sys
import time

from playwright.sync_api import sync_playwright

FAILS = []


def check(label, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'}  {label}{(' — ' + detail) if detail else ''}")
    if not ok:
        FAILS.append(label)


def open_settings(page):
    page.click("#btn-settings")
    page.wait_for_timeout(250)


def switch_to(page, name):
    page.evaluate(
        """async (n) => {
             const u = state.users.find(x => x.name === n);
             await api('/api/users/' + u.id + '/activate', { method: 'POST' });
             await afterProfileChange();
           }""",
        name,
    )
    page.wait_for_timeout(400)


def save_creds(page, client_id, secret):
    page.fill("#w-id", client_id)
    page.fill("#w-secret", secret)
    page.click("#w-form button[type=submit]")
    page.wait_for_timeout(500)


def strand_profile(db_path, name):
    """Leave `name` holding a refresh token with no registration behind it.

    This is what an older build could produce: tokens minted while the
    client id was still app-wide, then that id migrating onto someone else's
    profile. Forged directly in the database because the app itself can no
    longer reach this state.
    """
    import sqlite3

    c = sqlite3.connect(db_path, timeout=10)
    (uid,) = c.execute("select id from users where name = ?", (name,)).fetchone()
    for k in ("client_id", "client_secret"):
        c.execute("delete from settings where key = ?", (f"u{uid}.withings.{k}",))
    for k, v in (("access_token", "tok"), ("refresh_token", "ref"),
                 ("expires_at", "2030-01-01T00:00:00+00:00")):
        c.execute(
            "insert into settings (key, value) values (?, ?) "
            "on conflict(key) do update set value = excluded.value",
            (f"u{uid}.withings.{k}", v),
        )
    c.commit()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8788")
    ap.add_argument("--db", required=True, help="the server's SQLite file, for forging edge states")
    a = ap.parse_args()

    with sync_playwright() as p:
        b = p.chromium.launch()
        page = b.new_page()
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.goto(a.url)
        page.wait_for_timeout(900)

        # Second profile, so there is something to keep separate.
        page.evaluate(
            """async () => {
                 await api('/api/users', { method: 'POST',
                   headers: { 'content-type': 'application/json' },
                   body: JSON.stringify({ name: 'Wife' }) });
                 await afterProfileChange();
               }"""
        )
        page.wait_for_timeout(400)
        open_settings(page)

        # ── profile 1 saves its own registration ──────────────────────
        switch_to(page, "Me")
        check("fresh profile shows the setup steps open", page.eval_on_selector("#w-setup", "e => e.open"))
        save_creds(page, "ID-ME", "SEC-ME")
        check(
            "after saving, profile 1 reads as registered",
            "Registered" in page.inner_text("#w-sub"),
            page.inner_text("#w-sub"),
        )

        # ── profile 2 must start blank, not inherit ───────────────────
        switch_to(page, "Wife")
        check(
            "profile 2 starts unconfigured",
            "Not set up" in page.inner_text("#w-sub"),
            page.inner_text("#w-sub"),
        )
        check(
            "profile 2's client id box is empty, not profile 1's",
            page.input_value("#w-id") == "",
            repr(page.input_value("#w-id")),
        )
        # The heading is styled uppercase, so compare case-insensitively.
        check(
            "the section names whose Withings it is",
            page.inner_text("#w-who").strip().lower() == "· wife",
            repr(page.inner_text("#w-who")),
        )

        save_creds(page, "ID-WIFE", "SEC-WIFE")
        check(
            "profile 2 saves its own registration",
            "Registered" in page.inner_text("#w-sub"),
            page.inner_text("#w-sub"),
        )

        # ── neither overwrote the other ───────────────────────────────
        switch_to(page, "Me")
        check("profile 1 still has its own id", page.input_value("#w-id") == "ID-ME", page.input_value("#w-id"))
        switch_to(page, "Wife")
        check("profile 2 still has its own id", page.input_value("#w-id") == "ID-WIFE", page.input_value("#w-id"))

        # ── the disclosure must stay where the user put it ────────────
        # Open it by hand, then start the background link poll — the exact
        # situation where it used to snap shut a beat later.
        page.click("#w-setup-summary")
        page.wait_for_timeout(120)
        check("clicking the summary opens the details", page.eval_on_selector("#w-setup", "e => e.open"))

        page.evaluate("window.open = () => null")  # don't actually spawn the OAuth tab
        page.click("#w-link")
        page.wait_for_timeout(5000)  # two-second poll: several ticks land in here
        check(
            "it is still open after the link poll has run",
            page.eval_on_selector("#w-setup", "e => e.open"),
            "closed under the poll" if not page.eval_on_selector("#w-setup", "e => e.open") else "",
        )

        # And a typed-but-unsaved edit survives a poll tick too.
        page.fill("#w-id", "TYPING-IN-PROGRESS")
        page.wait_for_timeout(4500)
        check(
            "a half-typed client id isn't overwritten mid-poll",
            page.input_value("#w-id") == "TYPING-IN-PROGRESS",
            page.input_value("#w-id"),
        )

        # Closing it by hand must also stick.
        page.click("#w-setup-summary")
        page.wait_for_timeout(4500)
        check("closing it by hand sticks", not page.eval_on_selector("#w-setup", "e => e.open"))

        # ── switching profiles from the Settings rows ─────────────────
        # Closing Settings, switching in the topbar and coming back is three
        # steps for something the row is already showing you.
        page.reload()
        page.wait_for_timeout(900)
        open_settings(page)
        rows = page.locator("#profile-list .profile-tag")
        check("every profile row is a button", rows.count() == 2, f"{rows.count()} rows")
        # Row order follows creation order, so row 0 is "Me".
        active_before = page.evaluate("state.users.find(u => u.active).name")
        target = "Wife" if active_before == "Me" else "Me"
        page.locator("#profile-list .profile-tag", has_text=target).click()
        page.wait_for_timeout(700)
        check(
            "clicking a profile row switches to it",
            page.evaluate("state.users.find(u => u.active).name") == target,
            page.evaluate("state.users.find(u => u.active).name"),
        )
        check("Settings stays open through the switch", page.eval_on_selector("#settings", "e => e.open"))
        check(
            "the Withings card follows the switch",
            page.inner_text("#w-who").strip().lower() == f"· {target.lower()}",
            repr(page.inner_text("#w-who")),
        )
        check(
            "the switched-to row is marked current",
            page.locator("#profile-list .profile-tag.on").inner_text().strip().lower().startswith(target.lower()),
        )

        # ── tokens with no registration behind them ──────────────────
        # Forge the exact state an older build could leave: a refresh token
        # on a profile whose client id and secret are gone.
        page.evaluate(
            """async () => {
                 const u = state.users.find(x => x.name === 'Wife');
                 if (!u.active) { await api('/api/users/' + u.id + '/activate', {method:'POST'}); }
                 await afterProfileChange();
               }"""
        )
        page.wait_for_timeout(400)
        strand_profile(a.db, "Wife")
        page.evaluate("loadWithings()")
        page.wait_for_timeout(600)
        check("a linked profile with no registration warns", not page.eval_on_selector("#w-warn", "e => e.hidden"))
        check(
            "and says so in the status line",
            "not registered" in page.inner_text("#w-sub").lower(),
            page.inner_text("#w-sub"),
        )
        check("and opens the registration form", page.eval_on_selector("#w-setup", "e => e.open"))

        # Saving a registration must clear those unrefreshable tokens.
        save_creds(page, "ID-WIFE-NEW", "SEC-WIFE-NEW")
        check("saving a new registration drops the stale tokens", page.eval_on_selector("#w-warn", "e => e.hidden"))
        check(
            "and the card asks you to connect",
            "Registered" in page.inner_text("#w-sub"),
            page.inner_text("#w-sub"),
        )

        check("no uncaught JS errors", not errors, "; ".join(errors[:3]))
        b.close()

    print()
    if FAILS:
        print(f"{len(FAILS)} failed: {', '.join(FAILS)}")
        sys.exit(1)
    print("all checks passed")


if __name__ == "__main__":
    main()
