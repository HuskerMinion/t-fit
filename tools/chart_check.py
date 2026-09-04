"""Live checks for the legend toggles and the day stepper.

Reuses fat_check's seeding, then drives the real UI: clicking legend entries
must add and remove series (and persist), and stepping back through weigh-ins
must move every tile together rather than leaving a half-rewound row.
"""
import argparse
import sys

from playwright.sync_api import sync_playwright

from fat_check import check, seed, FAILS


def series_present(page, cls):
    return page.locator(f"#chart .{cls}").count() > 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--db", required=True)
    a = ap.parse_args()
    seed(a.db)

    with sync_playwright() as p:
        b = p.chromium.launch()
        page = b.new_page(viewport={"width": 1200, "height": 900})
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.goto(a.url)
        page.wait_for_timeout(1200)
        # A goal gives the pace and target lines something to draw.
        page.evaluate(
            """async () => {
                 await api('/api/goals', { method: 'POST',
                   headers: { 'content-type': 'application/json' },
                   body: JSON.stringify({ target_lb: 180, target_date: '2026-06-01' }) });
                 await load();
                 state.days = null; applyRange();
               }"""
        )
        page.wait_for_timeout(900)

        # ── legend toggles ───────────────────────────────────────────
        for name, cls in [("fat", "fat-line"), ("trend", "trend-line"),
                          ("pace", "pace-line"), ("target", "goal-line"), ("dots", "dot")]:
            btn = page.locator(f'#legend .lg[data-series="{name}"]')
            check(f"{name}: legend entry exists", btn.count() == 1)
            check(f"{name}: drawn to begin with", series_present(page, cls))
            btn.click()
            page.wait_for_timeout(350)
            check(f"{name}: clicking removes it", not series_present(page, cls))
            check(f"{name}: its legend entry stays, dimmed",
                  not btn.is_hidden() and "off" in (btn.get_attribute("class") or ""),
                  btn.get_attribute("class"))
            btn.click()
            page.wait_for_timeout(350)
            check(f"{name}: clicking again brings it back", series_present(page, cls))

        # Hiding the target must free the axis, not leave a gap where it was.
        before = page.locator("#chart .axis-text").first.text_content()
        page.locator('#legend .lg[data-series="target"]').click()
        page.wait_for_timeout(350)
        after = page.locator("#chart .axis-text").first.text_content()
        check("hiding the target rescales the weight axis", before != after, f"{before} → {after}")
        page.locator('#legend .lg[data-series="target"]').click()
        page.wait_for_timeout(350)

        # Persisted, not just in-memory.
        page.locator('#legend .lg[data-series="fat"]').click()
        page.wait_for_timeout(500)
        page.reload()
        page.wait_for_timeout(1400)
        page.evaluate("state.days = null; applyRange();")
        page.wait_for_timeout(400)
        check("a toggle survives a reload", not series_present(page, "fat-line"))
        page.locator('#legend .lg[data-series="fat"]').click()
        page.wait_for_timeout(500)
        check("and can be restored after one", series_present(page, "fat-line"))

        # ── the current tile carries body fat ────────────────────────
        check("body fat shows beside the current weight",
              not page.eval_on_selector("#s-current-fat", "e => e.hidden"))
        fat_now = page.inner_text("#s-current-fat")
        check("and reads as a percentage", fat_now.endswith("%"), fat_now)

        # ── stepping back ────────────────────────────────────────────
        check("the day nav is visible", not page.eval_on_selector("#daynav", "e => e.hidden"))
        check("it starts on the latest", page.inner_text("#day-label") == "Latest weigh-in",
              page.inner_text("#day-label"))
        check("you can't step past the newest day", page.eval_on_selector("#day-next", "e => e.disabled"))

        latest = {k: page.inner_text(k) for k in ["#s-current", "#s-trend", "#s-30", "#s-rate", "#s-goal"]}
        page.click("#day-prev")
        page.wait_for_timeout(700)
        check("the label names the day you moved to",
              page.inner_text("#day-label") != "Latest weigh-in", page.inner_text("#day-label"))
        # The label is styled uppercase, so compare case-insensitively.
        check("the hero tile relabels itself",
              page.inner_text("#s-current-label").lower() == "that day",
              page.inner_text("#s-current-label"))
        check("a way back appears", not page.eval_on_selector("#day-today", "e => e.hidden"))

        # Step back to the very first weigh-in. The seeded weights fall in a
        # straight line, so adjacent days share a 30-day change and a rate —
        # only the start of the log, where there isn't 30 days of history
        # behind it yet, proves those tiles are really being recomputed.
        page.evaluate("selectDay(state.entries[0].date)")
        page.wait_for_timeout(900)
        past = {k: page.inner_text(k) for k in ["#s-current", "#s-trend", "#s-30", "#s-rate", "#s-goal"]}
        for k in ["#s-current", "#s-trend", "#s-goal"]:
            check(f"{k} follows the selected day", latest[k] != past[k], f"{latest[k]} → {past[k]}")
        check("30-day change is blank when there's no 30 days behind it",
              past["#s-30"] == "—", past["#s-30"])
        check("so is the rate", past["#s-rate"] == "—", past["#s-rate"])

        # And the figures are the server's, not a second implementation here.
        srv = page.evaluate("api('/api/stats?as_of=' + state.entries[0].date)")
        check("the hero weight matches the server's as-of figure",
              past["#s-current"] == f"{srv['current']:.1f}", f"{past['#s-current']} vs {srv['current']}")
        check("the count is right for that day", srv["count"] == 1, srv["count"])

        # Now somewhere with history behind it, where every tile has a value.
        page.evaluate("selectDay(state.entries[45].date)")
        page.wait_for_timeout(900)
        mid = {k: page.inner_text(k) for k in ["#s-current", "#s-trend", "#s-30", "#s-rate"]}
        for k in mid:
            check(f"{k} has a real value mid-log", mid[k] != "—", mid[k])
        srv = page.evaluate("api('/api/stats?as_of=' + state.entries[45].date)")
        check("mid-log weight matches the server too",
              mid["#s-current"] == f"{srv['current']:.1f}", f"{mid['#s-current']} vs {srv['current']}")

        # Put it back where the keyboard checks expect it.
        page.evaluate("selectDay(state.entries[state.entries.length - 32].date)")
        page.wait_for_timeout(700)

        # The chart must not have moved — that was the whole point.
        check("the chart still shows the full range",
              page.inner_text("#chart-sub").endswith("Mar 31, 2026"), page.inner_text("#chart-sub"))

        # ── keyboard ─────────────────────────────────────────────────
        at = page.inner_text("#day-label")
        page.keyboard.press("ArrowRight")
        page.wait_for_timeout(600)
        check("the right arrow key steps forward", page.inner_text("#day-label") != at,
              f"{at} → {page.inner_text('#day-label')}")
        page.keyboard.press("ArrowLeft")
        page.wait_for_timeout(600)
        check("and the left arrow steps back", page.inner_text("#day-label") == at,
              page.inner_text("#day-label"))

        # Typing in a field must not scrub the day.
        page.click("#f-memo")
        page.keyboard.press("ArrowLeft")
        page.wait_for_timeout(400)
        check("arrow keys are inert while typing in a field",
              page.inner_text("#day-label") == at, page.inner_text("#day-label"))

        # ── back to the present ──────────────────────────────────────
        page.click("#day-today")
        page.wait_for_timeout(700)
        check("back to latest restores the label", page.inner_text("#day-label") == "Latest weigh-in")
        now = {k: page.inner_text(k) for k in latest}
        check("and restores every figure", now == latest, f"{now} vs {latest}")

        # ── clicking a point ─────────────────────────────────────────
        box = page.locator("#chart").bounding_box()
        page.mouse.click(box["x"] + box["width"] * 0.2, box["y"] + box["height"] / 2)
        page.wait_for_timeout(700)
        check("clicking a weigh-in selects that day",
              page.inner_text("#day-label") != "Latest weigh-in", page.inner_text("#day-label"))

        # ── tooltip extras ───────────────────────────────────────────
        page.mouse.move(box["x"] + box["width"] * 0.3, box["y"] + box["height"] / 2)
        page.wait_for_timeout(400)
        tip = page.inner_text("#tip")
        check("the tooltip reports muscle", "muscle" in tip.lower(), tip.replace("\n", " | "))

        check("no uncaught JS errors", not errors, "; ".join(errors[:3]))
        b.close()

    print()
    if FAILS:
        print(f"{len(FAILS)} failed: {', '.join(FAILS)}")
        sys.exit(1)
    print("all checks passed")


if __name__ == "__main__":
    main()
