// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useDialogFocus } from "./useDialogFocus";
function Modal({ close, children }: { close: () => void; children?: React.ReactNode }) {
  const ref = useDialogFocus(close);
  return <div ref={ref} tabIndex={-1} role="dialog" aria-modal="true"><button>first</button><button disabled>disabled</button><button>last</button>{children}</div>;
}
afterEach(cleanup);
it("Tab/Shift+Tab stay inside the dialog, Escape closes, focus returns to the trigger", () => {
  const trigger = document.createElement("button"); document.body.append(trigger); trigger.focus();
  const close = vi.fn(); const ui = render(<Modal close={close} />);
  const first = screen.getByText("first"), last = screen.getByText("last");
  expect(document.activeElement).toBe(first);
  fireEvent.keyDown(first, { key: "Tab", shiftKey: true }); expect(document.activeElement).toBe(last);
  fireEvent.keyDown(last, { key: "Tab" }); expect(document.activeElement).toBe(first);
  fireEvent.keyDown(first, { key: "Escape" }); expect(close).toHaveBeenCalledOnce();
  ui.unmount(); expect(document.activeElement).toBe(trigger); trigger.remove();
});
it("only the topmost modal consumes Escape", () => {
  const outer = vi.fn(), inner = vi.fn();
  render(<Modal close={outer}><Modal close={inner} /></Modal>);
  fireEvent.keyDown(document, { key: "Escape" });
  expect(inner).toHaveBeenCalledOnce(); expect(outer).not.toHaveBeenCalled();
});
