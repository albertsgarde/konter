// The gaps page: tables that sort by any column, and a switch between the
// groups and the single transactions. Without it, the page shows both views,
// as the groups rank them and newest first.
(() => {
  const store = {
    get(k, d) { try { const v = localStorage.getItem("gaps:" + k); return v === null ? d : JSON.parse(v); } catch { return d; } },
    set(k, v) { try { localStorage.setItem("gaps:" + k, JSON.stringify(v)); } catch {} },
  };

  // A mapped share with one decimal, rounded down so that 100.0% means
  // everything is mapped: as `mapped_percent` in gaps.rs.
  const mappedPercent = (mapped, whole) =>
    whole ? (Math.floor((mapped * 1000) / whole) / 10).toFixed(1) + "%" : "-";

  // → mapped adds up down the table, so it's worked out again for whatever
  // order is shown. Mapping a group adds its money to the mapped money, except
  // likely transfers, which leave income and spending altogether. Amounts are
  // in øre, so the sums are exact.
  function addUp(table) {
    if (!("mapped" in table.dataset)) return;
    let mapped = Number(table.dataset.mapped), whole = Number(table.dataset.whole);
    for (const tr of table.tBodies[0].rows) {
      const transfer = Number(tr.dataset.transfer);
      mapped += Number(tr.dataset.gross) - transfer;
      whole -= transfer;
      tr.querySelector("td.after").textContent = mappedPercent(mapped, whole);
    }
  }

  // Click a heading with data-sort ("num" or "text") to sort by it, again to
  // turn the order around. A cell sorts by its data-v, else by its text. Ties
  // keep the order the rows came in. Empty text sorts last either way.
  function sortable(table) {
    const heads = [...table.tHead.rows[0].cells];
    const body = table.tBodies[0];
    const rows = [...body.rows];
    const [col, dir] = table.dataset.default.split(":");
    const fallback = { col: Number(col), dir };
    let sort = store.get("sort:" + table.id, fallback);
    if (!heads[sort.col]?.dataset.sort) sort = fallback;

    function apply() {
      const num = heads[sort.col].dataset.sort === "num";
      const sign = sort.dir === "asc" ? 1 : -1;
      const keyed = rows.map((tr, i) => {
        const td = tr.cells[sort.col];
        return { tr, i, k: td.dataset.v ?? td.textContent.trim() };
      });
      keyed.sort((a, b) => {
        if (!num && (a.k === "") !== (b.k === "")) return a.k === "" ? 1 : -1;
        const d = num ? Number(a.k) - Number(b.k) : a.k.localeCompare(b.k);
        return sign * d || a.i - b.i;
      });
      body.append(...keyed.map(r => r.tr));
      heads.forEach((th, i) => {
        th.querySelector(".arrow")?.remove();
        const on = i === sort.col;
        th.classList.toggle("sorted", on);
        if (on) {
          th.setAttribute("aria-sort", sort.dir === "asc" ? "ascending" : "descending");
          th.insertAdjacentHTML("beforeend", `<span class="arrow">${sort.dir === "asc" ? "▲" : "▼"}</span>`);
        } else {
          th.removeAttribute("aria-sort");
        }
      });
      addUp(table);
    }

    heads.forEach((th, i) => {
      if (!th.dataset.sort) return;
      th.addEventListener("click", () => {
        const first = th.dataset.first || (th.dataset.sort === "num" ? "desc" : "asc");
        sort = sort.col === i ? { col: i, dir: sort.dir === "asc" ? "desc" : "asc" } : { col: i, dir: first };
        store.set("sort:" + table.id, sort);
        apply();
      });
    });
    apply();
  }

  document.querySelectorAll("table.sortable").forEach(sortable);

  const view = document.getElementById("view");
  if (view) {
    const buttons = [...view.querySelectorAll("button[data-view]")];
    const show = v => {
      for (const b of buttons) {
        const on = b.dataset.view === v;
        b.setAttribute("aria-pressed", String(on));
        document.getElementById(b.dataset.view + "-view").hidden = !on;
      }
    };
    view.addEventListener("click", e => {
      const b = e.target.closest("button[data-view]");
      if (!b) return;
      store.set("view", b.dataset.view);
      show(b.dataset.view);
    });
    const saved = store.get("view", "groups");
    show(buttons.some(b => b.dataset.view === saved) ? saved : "groups");
    view.hidden = false;
  }
})();
