import type { SetStateAction } from "react";

export type Attachment = { key: number; file: Blob; url: string };
export type Sent = {
  text: string;
  wire?: string;
  thumbs?: string[];
  phase: "sending" | "sent" | "failed";
  queued?: boolean;
  failure?: string;
  stage?: "upload";
};
type State = { draft: string; images: Attachment[]; sent: Sent | null };

/** Page-memory state for one session. Async results survive card navigation (2026-10-11,
 * agora-kvj1); storing only drafts loses a failure received after the card unmounts. */
export class MobileComposer {
  private state: State = { draft: "", images: [], sent: null };
  private listeners = new Set<() => void>();
  private previews = new Set<string>();
  private nextKey = 0;
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };
  private set<K extends keyof State>(key: K, value: SetStateAction<State[K]>) {
    const next = typeof value === "function"
      ? (value as (previous: State[K]) => State[K])(this.state[key]) : value;
    this.state = { ...this.state, [key]: next };
    this.listeners.forEach((listener) => listener());
  }
  setDraft = (value: SetStateAction<string>) => this.set("draft", value);
  setImages = (value: SetStateAction<Attachment[]>) => this.set("images", value);
  setSent = (value: SetStateAction<Sent | null>) => this.set("sent", value);
  attach(file: Blob): Attachment {
    const url = URL.createObjectURL(file);
    this.previews.add(url);
    return { key: this.nextKey++, file, url };
  }
  release(url: string) {
    URL.revokeObjectURL(url);
    this.previews.delete(url);
  }
  dispose() {
    for (const url of this.previews) URL.revokeObjectURL(url);
    this.previews.clear();
    this.listeners.clear();
  }
}
