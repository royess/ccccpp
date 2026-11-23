// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { beforeEach, afterEach, it, expect, vi } from "vite-plus/test";
import { BatchRelayMessageModal } from "./BatchRelayMessageModal";
import type { GroupMeta, LedgerEvent } from "../../types";
const mock = vi.hoisted(() => ({ actors: vi.fn(), send: vi.fn() }));
vi.mock("../../services/api", () => ({
  fetchActors: (...args: unknown[]) => mock.actors(...args),
  relayMessage: (...args: unknown[]) => mock.send(...args),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ i18n: { language: "en" }, t: (key: string) => key }),
}));
vi.mock("../GroupCombobox", () => ({
  GroupCombobox: (p: {
    items: { value: string; label: string }[];
    value: string;
    onChange: (id: string) => void;
    disabled: boolean;
  }) => (
    <select
      aria-label="Destination"
      disabled={p.disabled}
      value={p.value}
      onChange={(e) => p.onChange(e.target.value)}
    >
      {p.items.map((item) => (
        <option key={item.value} value={item.value}>
          {item.label}
        </option>
      ))}
    </select>
  ),
}));
vi.mock("../LazyMarkdownRenderer", () => ({
  LazyMarkdownRenderer: ({ content }: { content: string }) => <div>{content}</div>,
}));
const messages: LedgerEvent[] = [
  { id: "first", kind: "chat.message", data: { text: "**First**" } },
  { id: "second", kind: "chat.message", data: { text: "Second" } },
];
const groups = [
  { group_id: "source", title: "Source" },
  { group_id: "one", title: "Group One" },
  { group_id: "two", title: "Group Two" },
] as GroupMeta[];
let host: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
const done = vi.fn();
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  mock.actors.mockReset();
  mock.send.mockReset();
  done.mockReset();
  mock.actors.mockImplementation(async (groupId: string) => ({
    ok: true,
    result: { actors: [{ id: `reader-${groupId}`, title: `Reader ${groupId}` }] },
  }));
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
async function button(text: string) {
  await act(async () =>
    Array.from(document.querySelectorAll("button"))
      .find((b) => b.textContent === text)!
      .click(),
  );
}
it("forwards a batch to multiple Groups with scoped recipients, partial results and stable retries", async () => {
  await act(async () =>
    root.render(
      <BatchRelayMessageModal
        isOpen
        srcGroupId="source"
        srcEvent={messages[0]}
        srcEvents={messages}
        groups={groups}
        onCancel={() => {}}
        onComplete={done}
      />,
    ),
  );
  await button("@all");
  await button("Reader one");
  await act(async () => {
    const select = document.querySelector<HTMLSelectElement>("select")!;
    select.value = "two";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await button("@all");
  await button("Reader two");
  mock.send
    .mockResolvedValueOnce({ ok: true, result: {} })
    .mockResolvedValueOnce({ ok: false, error: { code: "NETWORK_ERROR", message: "Interrupted" } })
    .mockResolvedValueOnce({ ok: true, result: {} })
    .mockResolvedValueOnce({ ok: true, result: {} });
  await button("Forward");
  expect(mock.send).toHaveBeenCalledTimes(4);
  expect(done).not.toHaveBeenCalled();
  expect(mock.send.mock.calls[0][2]).toEqual(["reader-one"]);
  expect(mock.send.mock.calls[1][2]).toEqual(["reader-two"]);
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain("3/4 accepted by CCCC");
  expect(document.querySelector<HTMLSelectElement>("select")!.disabled).toBe(true);
  const key = mock.send.mock.calls[1][5];
  mock.send.mockResolvedValue({ ok: true, result: {} });
  await button("Retry remaining");
  expect(mock.send).toHaveBeenCalledTimes(5);
  expect(mock.send.mock.calls[4][5]).toBe(key);
  expect(done).toHaveBeenCalledOnce();
});
it("keeps the single-message entry point and forwards it to multiple Groups", async () => {
  await act(async () =>
    root.render(
      <BatchRelayMessageModal
        isOpen
        srcGroupId="source"
        srcEvent={messages[0]}
        groups={groups}
        onCancel={() => {}}
        onComplete={done}
      />,
    ),
  );
  await act(async () => {
    const select = document.querySelector<HTMLSelectElement>("select")!;
    select.value = "two";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain(
    "1 messages × 2 Groups = 2 forwards",
  );
  mock.send.mockResolvedValue({ ok: true, result: {} });
  await button("Forward");
  expect(mock.send).toHaveBeenCalledTimes(2);
  expect(mock.send.mock.calls.map((call) => [call[0], call[3]])).toEqual([
    ["one", { groupId: "source", eventId: "first" }],
    ["two", { groupId: "source", eventId: "first" }],
  ]);
  expect(done).toHaveBeenCalledOnce();
});
