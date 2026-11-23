import { useChatObscuringViewOnPhone } from "../../hooks/useChatObscuringView";
import { createPortal } from "react-dom";
import { useEffect, useMemo, useState, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { Actor, ChatMessageData, GroupMeta, LedgerEvent } from "../../types";
import * as api from "../../services/api";
import { useModalA11y } from "../../hooks/useModalA11y";
import { Button } from "../ui/button";
import { Textarea } from "../ui/textarea";
import { GroupCombobox } from "../GroupCombobox";
import { LazyMarkdownRenderer } from "../LazyMarkdownRenderer";
import {
  prepareRelayBatch,
  runRelayBatch,
  type RelayOperation,
} from "../../features/messageRelay/relayBatch";

export interface BatchRelayMessageModalProps {
  isOpen: boolean;
  srcGroupId: string;
  srcEvent: LedgerEvent | null;
  srcEvents?: LedgerEvent[];
  groups: GroupMeta[];
  onCancel: () => void;
  onComplete: () => void | Promise<void>;
}
export function BatchRelayMessageModal({
  isOpen,
  srcGroupId,
  srcEvent,
  srcEvents,
  groups,
  onCancel,
  onComplete,
}: BatchRelayMessageModalProps) {
  const { i18n, t } = useTranslation("modals");
  useChatObscuringViewOnPhone(isOpen);
  const zh = i18n.language.startsWith("zh");
  const source = useMemo(
    () => (srcEvents?.length ? srcEvents : srcEvent ? [srcEvent] : []),
    [srcEvents, srcEvent],
  );
  const destinations = useMemo(
    () =>
      groups
        .filter((g) => g.group_id !== srcGroupId)
        .map((g) => ({ value: g.group_id, label: g.title || g.topic || g.group_id })),
    [groups, srcGroupId],
  );
  const first = destinations[0]?.value || "";
  const [current, setCurrent] = useState(first);
  const [targets, setTargets] = useState<Record<string, string[]>>(() =>
    first ? { [first]: ["@all"] } : {},
  );
  const [actors, setActors] = useState<Actor[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState(false);
  const [note, setNote] = useState("");
  const [plan, setPlan] = useState<RelayOperation[] | null>(null);
  const [running, setRunning] = useState(false);
  const inFlight = useRef(false);
  const stop = useRef(false);
  useEffect(
    () => () => {
      stop.current = true;
    },
    [],
  );
  const [error, setError] = useState("");
  const frozen = !!plan;
  const close = () => {
    if (!running) onCancel();
  };
  const { modalRef } = useModalA11y(isOpen, close);
  useEffect(() => {
    if (!isOpen || !current) return;
    let active = true;
    const abort = new AbortController();
    setLoading(true);
    setLoadError(false);
    setActors([]);
    void api
      .fetchActors(current, false, { signal: abort.signal })
      .then((response) => {
        if (!active) return;
        if (response.ok) setActors(response.result.actors || []);
        else setLoadError(true);
      })
      .catch(() => {
        if (active) setLoadError(true);
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
      abort.abort();
    };
  }, [current, isOpen]);
  function selectGroup(groupId: string) {
    if (current !== groupId) {
      setActors([]);
      setLoading(true);
    }
    setCurrent(groupId);
    setTargets((previous) => ({ ...previous, [groupId]: previous[groupId] || ["@all"] }));
  }
  function removeGroup(groupId: string) {
    const next = { ...targets };
    delete next[groupId];
    setTargets(next);
    if (current === groupId) {
      setActors([]);
      setLoading(true);
      setCurrent(Object.keys(next)[0] || "");
    }
  }
  function toggleToken(token: string) {
    setTargets((previous) => {
      const tokens = previous[current] || [];
      return {
        ...previous,
        [current]: tokens.includes(token) ? tokens.filter((v) => v !== token) : [...tokens, token],
      };
    });
  }
  async function submit() {
    if (inFlight.current) return;
    setError("");
    let operations = plan;
    if (!operations) {
      try {
        operations = prepareRelayBatch(
          srcGroupId,
          source,
          Object.entries(targets).map(([groupId, to]) => ({ groupId, to })),
          note,
        );
      } catch (e) {
        setError(e instanceof Error ? e.message : "Unable to prepare forwarding.");
        return;
      }
      setPlan(operations);
    }
    inFlight.current = true;
    stop.current = false;
    setRunning(true);
    try {
      const result = await runRelayBatch(
        operations,
        (op) =>
          api.relayMessage(
            op.groupId,
            op.text,
            op.to,
            { groupId: op.sourceGroupId, eventId: op.eventId },
            op.quote,
            op.clientId,
            AbortSignal.timeout(20000),
          ),
        setPlan,
        () => !stop.current,
      );
      if (result.every((op) => op.status === "accepted")) await onComplete();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Unable to finish forwarding.");
    } finally {
      inFlight.current = false;
      setRunning(false);
    }
  }
  if (!isOpen) return null;
  const labels = zh
    ? { pending: "待发送", accepted: "CCCC 已接受", failed: "失败", unknown: "结果待确认" }
    : {
        pending: "Pending",
        accepted: "Accepted by CCCC",
        failed: "Failed",
        unknown: "Outcome unknown",
      };
  const selectedTokens = targets[current] || [];
  const count = source.length * Object.keys(targets).length;
  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-3"
      role="dialog"
      aria-modal="true"
      aria-labelledby="relay-modal-title"
      onPointerDown={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <div
        ref={modalRef}
        className="flex w-full max-w-3xl min-h-0 max-h-[calc(100dvh-24px)] flex-col overflow-hidden rounded-2xl border border-[var(--glass-border-subtle)] bg-[var(--color-bg-primary)] text-[var(--color-text-primary)] shadow-2xl"
      >
        <div className="shrink-0 border-b border-[var(--glass-border-subtle)] px-5 py-4">
          <h2 id="relay-modal-title" className="text-lg font-semibold">
            {zh ? "转发消息" : "Forward messages"}
          </h2>
          <p className="mt-1 text-xs text-[var(--color-text-secondary)]">
            {zh
              ? `${source.length} 条消息 × ${Object.keys(targets).length} 个工作组 = ${count} 次转发`
              : `${source.length} messages × ${Object.keys(targets).length} Groups = ${count} forwards`}
          </p>
        </div>
        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
          <details
            open={source.length === 1}
            className="rounded-xl border border-[var(--glass-border-subtle)] p-3"
          >
            <summary className="cursor-pointer text-sm font-medium">
              {zh ? "原消息预览" : "Source message preview"} · {source.length}
            </summary>
            <div className="mt-2 max-h-[30vh] space-y-3 overflow-y-auto" data-relay-source-preview>
              {source.map((event) => {
                const data = (event.data || {}) as ChatMessageData;
                return (
                  <article
                    key={event.id}
                    className="min-w-0 border-t border-[var(--glass-border-subtle)] pt-2 text-sm"
                  >
                    <p className="mb-1 text-xs text-[var(--color-text-secondary)]">
                      {event.by || "—"} · {event.ts ? new Date(event.ts).toLocaleString() : ""}
                    </p>
                    <LazyMarkdownRenderer
                      content={data.text || ""}
                      fallback={
                        <p className="whitespace-pre-wrap break-words">{data.text || ""}</p>
                      }
                    />
                    {!!data.attachments?.length && (
                      <p className="mt-1 text-xs">
                        {zh
                          ? "附件保留在原消息中；转发保留来源链接。"
                          : "Attachments remain in the original message; source links are retained."}
                      </p>
                    )}
                  </article>
                );
              })}
            </div>
          </details>
          <div>
            <label className="mb-2 block text-sm font-medium">
              {zh ? "添加或切换目标工作组" : "Add or edit destination Groups"}
            </label>
            <GroupCombobox
              items={destinations}
              value={current}
              onChange={selectGroup}
              placeholder={t("relay.destinationGroup")}
              searchPlaceholder={t("relay.searchDestinationGroup", {
                defaultValue: "Search destination groups...",
              })}
              emptyText={t("relay.noMatchingGroups", { defaultValue: "No matching groups" })}
              ariaLabel={t("relay.destinationGroup")}
              triggerClassName="glass-input min-h-10 w-full rounded-lg px-3 py-2 text-sm"
              disabled={frozen || !destinations.length}
            />
            <div className="mt-2 flex flex-wrap gap-2">
              {Object.keys(targets).map((groupId) => (
                <div
                  key={groupId}
                  className={`inline-flex min-w-0 items-center rounded-lg border border-[var(--glass-border-subtle)] text-xs ${current === groupId ? "ring-1 ring-[var(--color-text-secondary)]" : ""}`}
                >
                  <button
                    type="button"
                    className="min-h-9 max-w-56 truncate px-2"
                    aria-pressed={current === groupId}
                    disabled={frozen}
                    onClick={() => selectGroup(groupId)}
                  >
                    {groups.find((g) => g.group_id === groupId)?.title || groupId}
                  </button>
                  <button
                    type="button"
                    className="min-h-9 px-2"
                    disabled={frozen}
                    aria-label={`${zh ? "移除目标" : "Remove destination"}: ${groupId}`}
                    onClick={() => removeGroup(groupId)}
                  >
                    ×
                  </button>
                </div>
              ))}
            </div>
          </div>
          {current && (
            <div>
              <p className="mb-2 text-sm font-medium">
                {zh ? "当前工作组的收件人" : "Recipients for this Group"}
              </p>
              <div className="flex flex-wrap gap-2">
                {["@all", "@foreman", "@peers", ...actors.map((a) => a.id)].map((token) => (
                  <button
                    type="button"
                    key={token}
                    className={`min-h-9 rounded-full border px-3 text-xs ${selectedTokens.includes(token) ? "border-current font-semibold" : "border-[var(--glass-border-subtle)]"}`}
                    aria-pressed={selectedTokens.includes(token)}
                    disabled={frozen || (loading && !token.startsWith("@"))}
                    onClick={() => toggleToken(token)}
                  >
                    {actors.find((a) => a.id === token)?.title || token}
                  </button>
                ))}
              </div>
              <p className="mt-1 text-xs text-[var(--color-text-secondary)]">
                {selectedTokens.length ? selectedTokens.join(", ") : "@foreman"}
              </p>
              {loading && (
                <p role="status" className="mt-1 text-xs">
                  {zh ? "正在读取 Agent…" : "Loading Agents…"}
                </p>
              )}
              {loadError && (
                <p role="status" className="mt-1 text-xs">
                  {zh
                    ? "Agent 列表暂不可用，仍可使用预设收件人。"
                    : "Agent list unavailable; preset recipients remain available."}
                </p>
              )}
            </div>
          )}
          <div>
            <label className="mb-2 block text-sm" htmlFor="relay-note">
              {t("relay.noteLabel")}
            </label>
            <Textarea
              id="relay-note"
              value={note}
              onChange={(event) => setNote(event.target.value)}
              rows={3}
              disabled={frozen}
            />
          </div>
          {plan && (
            <div
              role="status"
              className="space-y-2 rounded-xl border border-[var(--glass-border-subtle)] p-3 text-xs"
            >
              <strong>
                {plan.filter((op) => op.status === "accepted").length}/{plan.length}{" "}
                {zh ? "已被 CCCC 接受" : "accepted by CCCC"}
              </strong>
              {plan.map((op) => (
                <p key={op.clientId} className="break-words">
                  {groups.find((g) => g.group_id === op.groupId)?.title || op.groupId} ·{" "}
                  {zh ? "消息" : "Message"}{" "}
                  {source.findIndex((event) => event.id === op.eventId) + 1} · {labels[op.status]}
                  {op.error ? ` · ${op.error}` : ""}
                </p>
              ))}
              <p>
                {zh
                  ? "重试仅处理未确认项，使用原去重标识；已接受项不会重复转发。"
                  : "Retries keep the same deduplication keys and skip accepted forwards."}
              </p>
            </div>
          )}
          {error && (
            <p role="alert" className="text-sm text-red-600">
              {error}
            </p>
          )}
        </div>
        <div className="flex shrink-0 justify-end gap-3 border-t border-[var(--glass-border-subtle)] px-5 py-3">
          {running && (
            <Button
              variant="secondary"
              onClick={() => {
                stop.current = true;
              }}
            >
              {zh ? "停止剩余转发" : "Stop remaining"}
            </Button>
          )}
          <Button variant="secondary" onClick={close} disabled={running}>
            {t("common:cancel")}
          </Button>
          <Button
            onClick={() => void submit()}
            disabled={running || !source.length || !Object.keys(targets).length}
          >
            {running
              ? t("relay.sending")
              : plan
                ? zh
                  ? "重试剩余项"
                  : "Retry remaining"
                : zh
                  ? "转发"
                  : "Forward"}
          </Button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
