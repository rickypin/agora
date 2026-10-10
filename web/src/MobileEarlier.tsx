/**
 * 会话卡最近一轮上方的「更早 N 轮」（A53，agora-2mff；MISSION §6.9；docs/spec/ux.md「会话页」）。
 *
 * 数据是节点自记的有界轮次日志（`GET /api/sessions/:id/turns`，ADR-002 D12）：每轮只有人说的话与
 * agent 的最终回复，没有工具过程。默认收起，展开也只到 3 轮——这不是消息流：没有翻页、没有「加载
 * 更多」、没有搜索（A52 的禁止名单不放宽，testid / 类名一律 `mobile-earlier*`）。
 *
 * 哪一轮算「最近一轮」：卡片的 ❯ 气泡是行上的 `prompt`，即最后一条**人说的** `prompt.submitted`。
 * 宿主注入的 prompt（`prompt.injected`）也会在日志里开一轮，所以锚点是「最后一个有人话的轮」，
 * 不是简单的最后一轮——反例：人说完「跑一下」之后宿主注入了一轮，`slice(0, -1)` 会把「跑一下」
 * 那一轮当成「更早」，与下面的 ❯ 气泡画两遍。锚点之后的注入轮属于最近一轮（它的回复就是卡片上
 * 那条 ↳），也不进「更早」。
 *
 * 取数失败、老节点（GET 落到 SPA 兜底的空 404）、还没记过：一律不占位、不报错。
 */
import { useEffect, useState } from "react";
import type { SessionApi, TurnInfo } from "./api";
import type { SessionRow } from "./events";
import { displayPrompt } from "./imageAttach";
import { MarkdownView } from "./MarkdownView";
import { str } from "./SessionRow";

/** 最多展开几轮（MISSION §6.9 / A53）。 */
export const EARLIER_MAX = 3;
/** 一次取几轮：锚点之后可能还挂着几轮注入的，多取几轮才保证锚点之前凑得满 3 轮。 */
export const EARLIER_FETCH = 8;
/** 每轮的人话 / 回复默认露出的行数（与卡片上最新回复同一个折叠，ux.md：手机端默认折 6 行）。 */
const FOLD = 6;

/** 锚点（最后一个有人话的轮）之前的最近 [`EARLIER_MAX`] 轮，旧在前。 */
export function earlierTurns(turns: readonly TurnInfo[]): TurnInfo[] {
  let anchor = turns.length - 1;
  for (let i = turns.length - 1; i >= 0; i--) {
    if (turns[i].prompt !== null) {
      anchor = i;
      break;
    }
  }
  return turns.slice(0, Math.max(0, anchor)).slice(-EARLIER_MAX);
}

/** 这一轮没有正常的文字回复时说一句为什么（有回复的 done 轮返回 null）。 */
function outcomeNote(t: TurnInfo): string | null {
  if (t.outcome === "failed") return t.failure ? `这一轮失败了：${t.failure}` : "这一轮失败了";
  if (t.outcome === "no_reply") return "没有收到这一轮的回复（下一轮已经开始）";
  if (t.reply === null || t.reply.trim() === "") return "这一轮没有文字回复";
  return null;
}

function fold(text: string, open: boolean): { shown: string; folded: boolean } {
  const lines = text.split("\n");
  return lines.length > FOLD && !open ? { shown: lines.slice(0, FOLD).join("\n"), folded: true } : { shown: text, folded: false };
}

function EarlierTurn({ turn, open, onOpen }: { turn: TurnInfo; open: boolean; onOpen: () => void }) {
  const prompt = turn.prompt === null ? null : fold(displayPrompt(turn.prompt), open);
  const reply = turn.reply === null || turn.reply.trim() === "" ? null : fold(turn.reply, open);
  const note = outcomeNote(turn);
  return (
    <li className="mobile-earlier-turn" data-testid="mobile-earlier-turn">
      {prompt ? (
        <div className="mobile-bubble user" data-testid="mobile-earlier-user">
          <span className="mobile-bubble-mark" aria-hidden="true">
            你
          </span>
          <span>{prompt.shown}</span>
        </div>
      ) : (
        // 没有人话的轮只有两种来路：宿主注入（`injected`），或日志开始记录时这一轮已经在跑——
        // 升级到带轮次日志的版本那一刻每个会话都会有一次（2026-10-10 真机反馈：「第一轮的用户输入
        // 没有显示」）。后者不说一句，看上去就是「你」气泡丢了。
        <p className="mobile-note" data-testid="mobile-earlier-no-prompt">
          {turn.injected ? "宿主自己发起的一轮" : "这一轮开始时还没在记录，你说的话没有留下"}
        </p>
      )}
      {reply && (
        <div className="mobile-bubble agent" data-testid="mobile-earlier-reply">
          <span className="mobile-bubble-mark" aria-hidden="true">
            ↳
          </span>
          <div className="mobile-bubble-body">
            <MarkdownView text={reply.shown} />
          </div>
        </div>
      )}
      {note && (
        <p className="mobile-note" data-testid="mobile-earlier-note">
          {note}
        </p>
      )}
      {(prompt?.folded || reply?.folded) && (
        <button className="mobile-more-line" data-testid="mobile-earlier-expand" onClick={onOpen}>
          展开这一轮
        </button>
      )}
    </li>
  );
}

export function MobileEarlier({ row, api }: { row: SessionRow; api: SessionApi }) {
  const [turns, setTurns] = useState<TurnInfo[]>([]);
  const [open, setOpen] = useState(false);
  const [unfolded, setUnfolded] = useState<ReadonlySet<string>>(new Set());
  useEffect(() => {
    setTurns([]);
    setOpen(false);
    setUnfolded(new Set());
  }, [row.id]);
  // 重新取的时机：换行、❯ 变了（新的一轮把上一轮挤进「更早」）、状态变了（同一句话再说一遍时 ❯
  // 不变，但它会经过 running）。离线的行不取——转发必然失败，留着上次取到的。
  const prompt = str(row.prompt);
  const stale = row.stale === true;
  useEffect(() => {
    if (stale) return;
    let live = true;
    api.turns(row.id, EARLIER_FETCH).then(
      (r) => {
        if (live) setTurns(r.ok && Array.isArray(r.value?.turns) ? r.value.turns : []);
      },
      () => {
        if (live) setTurns([]);
      },
    );
    return () => {
      live = false;
    };
  }, [api, row.id, prompt, row.status, stale]);

  const earlier = earlierTurns(turns);
  if (earlier.length === 0) return null;
  return (
    <div className="mobile-earlier" data-testid="mobile-earlier">
      <button className="mobile-earlier-toggle" data-testid="mobile-earlier-toggle" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        {open ? `收起更早 ${earlier.length} 轮` : `更早 ${earlier.length} 轮`}
      </button>
      {open && (
        <ol className="mobile-earlier-list" data-testid="mobile-earlier-list">
          {earlier.map((t, i) => {
            // 同一秒里的两轮 started_at 相同：键带上位置，免得展开一轮把另一轮也展开。
            const key = `${t.started_at ?? ""}-${i}`;
            return <EarlierTurn key={key} turn={t} open={unfolded.has(key)} onOpen={() => setUnfolded(new Set(unfolded).add(key))} />;
          })}
        </ol>
      )}
    </div>
  );
}
