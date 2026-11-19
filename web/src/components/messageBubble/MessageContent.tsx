import { useCallback } from "react";
import { useGroupStore } from "../../stores/useGroupStore";
import { messageFileHref } from "../workspace/workspaceReportLink";
import { classNames } from "../../utils/classNames";
import { LazyMarkdownRenderer } from "../LazyMarkdownRenderer";

function PlainMessageText({ text, className }: { text: string; className?: string }) {
  return (
    <div
      className={classNames(
        "break-words whitespace-pre-wrap text-[var(--color-text-primary)] [overflow-wrap:anywhere]",
        className,
      )}
    >
      {text}
    </div>
  );
}

export function MessageContent(props: {
  groupId: string;
  fallbackText: string;
  shouldRenderMarkdown: boolean;
  isDark: boolean;
}) {
  const root = useGroupStore(({ groupDoc }) =>
    groupDoc?.group_id === props.groupId
      ? groupDoc.scopes?.find((s) => s.scope_key === groupDoc.active_scope_key)?.url || ""
      : "",
  );
  const resolveUrl = useCallback((url: string, kind: "image" | "link") =>
    kind === "link" ? messageFileHref(url, props.groupId, root) : url, [props.groupId, root]);
  if (!props.shouldRenderMarkdown) {
    return <PlainMessageText text={props.fallbackText} className="max-w-full" />;
  }
  return (
    <LazyMarkdownRenderer
      content={props.fallbackText}
      isDark={props.isDark}
      invertText={false}
      resolveUrl={resolveUrl}
      enableMermaid
      className="max-w-full break-words text-[var(--color-text-primary)] [overflow-wrap:anywhere]"
      fallback={<PlainMessageText text={props.fallbackText} className="max-w-full" />}
    />
  );
}
