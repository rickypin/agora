/**
 * 浏览器通知（MISSION §6.6；A18；agora-dvh.11）。V1 桌面用 Notification API，Web Push 归 V2-1。
 *
 * 该不该发由服务端决定：`notification` 事件只在 RUNNING → WAITING / TURN_DONE / FINISHED / FAILED
 * 四种转换上来一条，`notifications.enabled` 关掉就一条也没有（src/events.rs）。这里只管三件事：
 * 权限（问一次，之后不再打扰）、弹（`tag = 会话 id`，同一会话多开几个标签页也只显示一条）、
 * 点击回到该行——WAITING 落到 Dashboard 就地回答，不是终端。
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

  constructor(
    private readonly deps: NotifierDeps,
    /** 点击：切到该会话；`status` 是转换后的状态，WAITING / TURN_DONE 的就地回答区随行展开。 */
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
    // 2026-09-04 实测 Chrome：同 tag 的新通知替换旧的而不是叠一条，所以 tag 用会话 id——一个会话
    // 只占通知中心一格，最新的转换赢；两个标签页各弹一次也只显示一条。系统通知（无 id）不合并。
    const tag = n.id ?? `agora-${Date.now()}`;
    const note = this.deps.create(n.title, { body: n.body, tag });
    this.shown += 1;
    note.onclick = () => {
      note.close();
      this.deps.focus();
      if (n.id) this.onOpen(n.id, n.status ?? null);
    };
    return true;
  }
}
