import { describe, expect, it } from "vitest";
import { emptyTabs, tabsReducer } from "./tabstate";

describe("tabsReducer", () => {
  it("open / activate / close keep a sensible active tab", () => {
    let s = tabsReducer(emptyTabs, { type: "open", id: "a" });
    s = tabsReducer(s, { type: "open", id: "b" });
    s = tabsReducer(s, { type: "open", id: "c" });
    expect(s).toEqual({ open: ["a", "b", "c"], active: "c" });
    s = tabsReducer(s, { type: "activate", id: "b" });
    s = tabsReducer(s, { type: "close", id: "b" });
    expect(s).toEqual({ open: ["a", "c"], active: "c" });
    s = tabsReducer(s, { type: "close", id: "c" });
    expect(s).toEqual({ open: ["a"], active: "a" });
    s = tabsReducer(s, { type: "close", id: "a" });
    expect(s).toEqual({ open: [], active: null });
  });

  it("re-opening an open tab only activates it; unchanged actions return the same state", () => {
    const s1 = tabsReducer(tabsReducer(emptyTabs, { type: "open", id: "a" }), { type: "open", id: "b" });
    const s2 = tabsReducer(s1, { type: "open", id: "b" });
    expect(s2).toBe(s1);
    expect(tabsReducer(s1, { type: "close", id: "zzz" })).toBe(s1);
    expect(tabsReducer(s1, { type: "prune", existing: new Set(["a", "b"]) })).toBe(s1);
  });

  it("prune closes tabs whose session vanished", () => {
    const s = tabsReducer(tabsReducer(emptyTabs, { type: "open", id: "a" }), { type: "open", id: "b" });
    expect(tabsReducer(s, { type: "prune", existing: new Set(["a"]) })).toEqual({ open: ["a"], active: "a" });
  });
});
