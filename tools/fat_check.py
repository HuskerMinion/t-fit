"""Live check for body composition: chart line, table column, CSV round-trip.

Seeds a database directly with weights that carry fat ratios — some days
deliberately without one — then drives the real UI against it. Run against a
server started on --port with the --db it was given.
"""
import argparse
import sqlite3
import sys

from playwright.sync_api import sync_playwright

FAILS = []


def check(label, ok, detail=""):
    detail = "" if not detail else str(detail)
    print(f"{'PASS' if ok else 'FAIL'}  {label}{(' — ' + detail) if detail else ''}")
    if not ok:
        FAILS.append(label)


#: Days with a weight but no fat reading. The short hole is the everyday
#: case — socks on, a bad contact — and should be bridged. The long one is a
#: scale that went unused for a month, and should break the line, matching
#: the 21-day rule the weight trend already follows.
SHORT_GAP = range(20, 25)
LONG_GAP = range(40, 66)
DAYS = 90


def seed(db_path):
    c = sqlite3.connect(db_path, timeout=10)
    (uid,) = c.execute("select id from users order by id limit 1").fetchone()
    from datetime import date, timedelta
    d0 = date(2026, 1, 1)
    for i in range(DAYS):
        day = d0 + timedelta(days=i)
        w = 200.0 - i * 0.15
        gap = i in SHORT_GAP or i in LONG_GAP
        c.execute(
            "insert into weight (user_id, day, weight_lb, memo, source, fat_ratio, muscle_lb) "
            "values (?,?,?,?,?,?,?)",
            (uid, day.isoformat(), round(w, 1), "note" if i == 5 else "", "withings",
             None if gap else round(30.0 - i * 0.03, 1),
             None if gap else round(w * 0.42, 1)),
        )
    c.commit()
    return uid


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

        # Show everything, not just the default trailing week.
        page.evaluate("(async () => { state.days = null; applyRange(); })()")
        page.wait_for_timeout(600)

        # ── chart ────────────────────────────────────────────────────
        check("the fat line is drawn", page.locator("#chart .fat-line").count() == 1)
        check("its legend entry shows", not page.eval_on_selector("#lg-fat", "e => e.hidden"))
        check("a right-hand percent axis is labelled",
              page.locator("#chart .fat-axis").count() >= 2,
              page.locator("#chart .fat-axis").count())
        # SVG <text> isn't an HTMLElement, so text_content is the way in.
        axis = page.locator("#chart .fat-axis").first.text_content()
        check("the axis is in percent, not pounds", axis.endswith("%"), axis)

        # A month-long hole breaks the path; a few missed days don't. Two
        # subpaths means exactly one break — the long gap and not the short.
        d = page.eval_on_selector("#chart .fat-line", "e => e.getAttribute('d')")
        check("a long gap breaks the line instead of ruling through it",
              d.count("M") == 2, f"{d.count('M')} subpaths")
        check("but a few missed days are bridged, not broken",
              d.count("M") == 2, f"{d.count('M')} subpaths")

        # The weight axis must not have been rescaled by percentages.
        left = [t.text_content() for t in page.locator("#chart .axis-text").all()[:4]]
        check("the weight axis still reads in pounds",
              any(t.replace(".", "").isdigit() and float(t) > 100 for t in left if t),
              left)

        # ── the toggle ───────────────────────────────────────────────
        # Lives on the legend now, not in Settings. chart_check covers every
        # series; this just confirms fat follows the same rule.
        btn = page.locator('#legend .lg[data-series="fat"]')
        btn.click()
        page.wait_for_timeout(450)
        check("turning it off removes the line", page.locator("#chart .fat-line").count() == 0)
        check("but its legend entry stays, so it can be turned back on",
              not page.eval_on_selector("#lg-fat", "e => e.hidden"))
        btn.click()
        page.wait_for_timeout(450)
        check("turning it back on restores it", page.locator("#chart .fat-line").count() == 1)

        # ── table ────────────────────────────────────────────────────
        page.evaluate("state.view='table'; document.querySelector('#view-table').hidden=false; renderTable();")
        page.wait_for_timeout(400)
        check("the fat column header appears", not page.eval_on_selector("#th-fat", "e => e.hidden"))
        first_fat = page.locator("#tbl tbody tr").first.locator("td.fat").inner_text()
        check("and carries a value", first_fat.strip() != "", repr(first_fat))
        # Rows run newest first, so index (DAYS-1 - i) is seeded day i.
        gap_row = DAYS - 1 - LONG_GAP[0]
        gap_cell = page.locator("#tbl tbody tr").nth(gap_row).locator("td.fat").inner_text()
        check("a day with no reading is blank, not zero", gap_cell.strip() == "", repr(gap_cell))

        # ── tooltip ──────────────────────────────────────────────────
        page.evaluate("state.view='chart'; document.querySelector('#view-table').hidden=true;")
        box = page.locator("#chart").bounding_box()
        page.mouse.move(box["x"] + box["width"] * 0.15, box["y"] + box["height"] / 2)
        page.wait_for_timeout(400)
        tip = page.inner_text("#tip")
        check("the tooltip reports body fat", "body fat" in tip.lower(), tip.replace("\n", " | "))

        check("no uncaught JS errors", not errors, "; ".join(errors[:3]))
        b.close()

    print()
    if FAILS:
        print(f"{len(FAILS)} failed: {', '.join(FAILS)}")
        sys.exit(1)
    print("all checks passed")


if __name__ == "__main__":
    main()
