/** Small, consistent touch-navigation icons; inherit the control's contrast. */
export function MobileIcon({ name }: { name: "inbox" | "plus" | "settings" | "search" | "arrow" | "check" | "image" }) {
  const paths = {
    inbox: <><path d="M4 4h16v16H4z"/><path d="M4 13h5l2 3h2l2-3h5"/></>,
    plus: <path d="M12 5v14M5 12h14"/>,
    settings: <><path d="M4 7h16M4 17h16"/><circle cx="9" cy="7" r="3"/><circle cx="15" cy="17" r="3"/></>,
    search: <><circle cx="10" cy="10" r="6"/><path d="m15 15 5 5"/></>,
    arrow: <path d="m9 5 7 7-7 7"/>,
    check: <path d="m5 12 4 4L19 6"/>,
    image: <><rect x="3" y="5" width="18" height="14" rx="2"/><circle cx="9" cy="10" r="1.6"/><path d="m4 18 5-5 4 4 3-3 4 4"/></>,
  };
  return <svg className="mobile-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name]}</svg>;
}
