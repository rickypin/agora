import { useLayoutEffect, useRef } from "react";

/** One keyboard boundary for all modal surfaces, including nested confirmations. */
export function useDialogFocus(onClose: () => void) {
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef(document.activeElement);
  const close = useRef(onClose);
  useLayoutEffect(() => { close.current = onClose; }, [onClose]);
  useLayoutEffect(() => {
    const el = root.current;
    if (!el) return;
    const targets = () => Array.from(el.querySelectorAll<HTMLElement>(
      'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex="0"]',
    )).filter((node) => !node.closest('[hidden], [inert]'));
    if (!el.contains(document.activeElement)) (targets()[0] ?? el).focus();
    const key = (e: KeyboardEvent) => {
      // 2026-10-09: a device revocation opens a second dialog. The outer one must
      // not trap the confirmation's Tab or consume its Escape.
      const dialogs = document.querySelectorAll('[role="dialog"][aria-modal="true"]');
      if (dialogs[dialogs.length - 1] !== el) return;
      if (e.key === "Escape") {
        e.preventDefault(); e.stopImmediatePropagation(); close.current(); return;
      }
      if (e.key !== "Tab") return;
      const nodes = targets(), first = nodes[0], last = nodes[nodes.length - 1];
      if (!first) { e.preventDefault(); el.focus(); return; }
      if (!el.contains(document.activeElement) || (e.shiftKey ? document.activeElement === first : document.activeElement === last)) {
        e.preventDefault(); (e.shiftKey ? last : first).focus();
      }
    };
    document.addEventListener("keydown", key, true);
    const previous = trigger.current;
    return () => {
      document.removeEventListener("keydown", key, true);
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
    };
  }, []);
  return root;
}
