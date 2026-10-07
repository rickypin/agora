/**
 * 浏览器通知（MISSION §6.6；A18；agora-dvh.11）。V1 桌面用 Notification API，Web Push 归 V2-1（agora-thc.7）。
 *
 * 该不该发由服务端决定：`notification` 事件只在 RUNNING → WAITING / TURN_DONE / FINISHED / FAILED
 * 四种转换上来一条，`notifications.enabled` 关掉就一条也没有（src/events.rs）。这里只管三件事：
 * 权限（问一次，之后不再打扰）、弹（`tag = 会话 id`，同一会话多开几个标签页也只显示一条）、
 * 点击回到该行并把焦点放进主区回答面板（WAITING / TURN_DONE；MISSION §6.6），不是终端。
 *
 * 还有一件反向的事（agora-gf7t）：误判被撤回（会话又活了）时要把还挂在屏上的「会话结束」
 * 通知收回来——服务端不会为这个转换再发一条通知，不收就永远留着。见 [`Notifier::retractTerminal`]。
 *
 * 与 DOM 解耦：`Notification` 构造器与权限经 [`NotifierDeps`] 注入，vitest 在 node 里跑。
 */

export interface NotificationLike {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  onclick: ((ev: any) => void) | null;
  close(): void;
}

export type Permission = "default" | "granted" | "denied" | "unsupported";

export interface NotifierDeps {
  permission: () => Permission;
  request: () => Promise<Permission>;
  create: (title: string, opts: { body: string; tag: string }) => NotificationLike;
  /** 点击通知时把窗口拉到前面（浏览器允许在通知点击回调里 focus）。 */
  focus: () => void;
}

export interface Incoming {
  id: string | null;
  title: string;
  body: string;
  status?: string | null;
}

/** 「会话结束」类通知的判据（agora-gf7t）：通知的 `status` 是转换后的状态，只有这两个算结束。 */
export function isEndStatus(status: string | null | undefined): boolean {
  return status === "finished" || status === "failed";
}

export function browserDeps(): NotifierDeps {
  const supported = typeof window !== "undefined" && "Notification" in window;
  return {
    permission: () => (supported ? (Notification.permission as Permission) : "unsupported"),
    request: async () => {
      if (!supported) return "unsupported";
      try {
        return (await Notification.requestPermission()) as Permission;
      } catch {
        // Safari 旧版是回调式 API，Promise 形态会抛；当作没答。
        return Notification.permission as Permission;
      }
    },
    create: (title, opts) => new Notification(title, opts),
    focus: () => window.focus(),
  };
}

export class Notifier {
  /** 弹过的通知数（测试断言用）。 */
  shown = 0;
  private seq = 0;
  /** 每个会话当前还挂着的那条：弹新的先关旧的。`end` 记它是不是「会话结束」类（可撤回）。 */
  private readonly open = new Map<string, { note: NotificationLike; end: boolean }>();

  constructor(
    private readonly deps: NotifierDeps,
    /** 点击：切到该会话并把焦点放进主区回答面板；`status` 是转换后的状态（WAITING / TURN_DONE；MISSION §6.6）。 */
    private readonly onOpen: (id: string, status: string | null) => void,
  ) {}

  permission(): Permission {
    return this.deps.permission();
  }

  /** 只在 `default` 时真的问；已答过（granted / denied）不再弹权限框（"权限请求 UX 一次"）。 */
  async request(): Promise<Permission> {
    if (this.deps.permission() !== "default") return this.deps.permission();
    return this.deps.request();
  }

  /** 返回是否弹了。没权限 / 不支持 → 静默；服务端已保证每个转换只来一条，这里不再去重。 */
  show(n: Incoming): boolean {
    if (this.deps.permission() !== "granted") return false;
    // 2026-09-04 人眼验收实测（macOS Chrome 152）：同 tag 的新通知只会静默替换通知中心里的旧条目，
    // 不再弹横幅，renotify 在 macOS 原生通知这条路上也不生效——用户只看到第一条。所以 tag 每条唯一，
    // "一个会话只占一格"改由我们自己做：弹新的之前把该会话上一条 close 掉。代价是多开几个标签页会
    // 各弹一条（同页面多标签本来就少见）。
    if (n.id) this.open.get(n.id)?.note.close();
    const tag = `${n.id ?? "agora"}#${++this.seq}`;
    const note = this.deps.create(n.title, { body: n.body, tag });
    if (n.id) this.open.set(n.id, { note, end: isEndStatus(n.status) });
    this.shown += 1;
    note.onclick = () => {
      note.close();
      if (n.id && this.open.get(n.id)?.note === note) this.open.delete(n.id);
      this.deps.focus();
      if (n.id) this.onOpen(n.id, n.status ?? null);
    };
    return true;
  }

  /**
   * 撤回该会话还挂着的那条**结束类**通知（agora-gf7t）：`ended_at` 被撤销、行又活了的那个转换
   * 不在服务端的四种通知转换里，没人会发新通知把它换掉，不收就永远留在屏上。
   * 等待类（needs input / finished its turn）不撤——它们等人点开，行短暂更新一下就撤掉是错的。
   * 返回是否撤了一条；没挂着、或挂着的是等待类都是 `false`。
   */
  retractTerminal(id: string): boolean {
    const open = this.open.get(id);
    if (!open || !open.end) return false;
    open.note.close();
    this.open.delete(id);
    return true;
  }
}
