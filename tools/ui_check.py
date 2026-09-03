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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8788")
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

        check("no uncaught JS errors", not errors, "; ".join(errors[:3]))
        b.close()

    print()
    if FAILS:
        print(f"{len(FAILS)} failed: {', '.join(FAILS)}")
        sys.exit(1)
    print("all checks passed")


if __name__ == "__main__":
    main()
