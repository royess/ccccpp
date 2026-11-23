// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { beforeEach, afterEach, it, expect, vi } from "vite-plus/test";
import { HumanAttention } from "./HumanAttention";
import { useGroupStore } from "../../stores/useGroupStore";
import { useUIStore } from "../../stores/useUIStore";
import { useModalStore } from "../../stores/useModalStore";
import {
  useWorkspaceNavigation,
  finishWorkspaceNavigation,
} from "../../stores/workspaceNavigation";
import type { AttentionItem } from "./model";
import type { GroupMeta } from "../../types";

const data = vi.hoisted(() => ({ state: {} as Record<string, unknown> }));
vi.mock("./useBrowserNotifications", () => ({ useBrowserNotifications: () => data.state }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ i18n: { language: "en" } }) }));
const items: AttentionItem[] = [
  {
    id: "permission",
    groupId: "g-two",
    actorId: "b",
    kind: "approval",
    title: "Agent Two",
    text: "Approve fixture command",
    ts: "2000-01-01T00:00:00Z",
  },
  {
    id: "reply",
    groupId: "g-one",
    actorId: "a",
    eventId: "event-one",
    kind: "reply",
    title: "Agent One",
    text: "Choose an option",
    ts: "2000-01-01T00:00:00Z",
  },
  {
    id: "task",
    groupId: "g-two",
    actorId: "b",
    taskId: "T001",
    kind: "task",
    title: "Agent Two",
    text: "Review task",
    ts: "",
  },
];
let root: ReturnType<typeof createRoot>;
let host: HTMLDivElement;
let previousGroup: ReturnType<typeof useGroupStore.getState>;
let previousUI: ReturnType<typeof useUIStore.getState>;
let previousModal: ReturnType<typeof useModalStore.getState>;
const select = vi.fn((id: string) =>
  useGroupStore.setState({ selectedGroupId: id, selectedGroupActorsHydrating: true }),
);
const openChat = vi.fn(async () => true);
const openTask = vi.fn();
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  previousGroup = useGroupStore.getState();
  previousUI = useUIStore.getState();
  previousModal = useModalStore.getState();
  useGroupStore.setState({
    groups: [
      { group_id: "g-one", title: "Group One" },
      { group_id: "g-two", title: "Group Two" },
    ] as GroupMeta[],
    selectedGroupId: "g-one",
    actors: [{ id: "a", running: true }],
    selectedGroupActorsHydrating: false,
    setSelectedGroupId: select,
    openChatWindow: openChat,
  });
  useModalStore.setState({ openContextTask: openTask });
  useUIStore.setState({ activeTab: "chat" });
  data.state = {
    items,
    on: false,
    status: "denied",
    error: "",
    staleGroups: [],
    limitedGroups: [],
    loading: false,
    refresh: vi.fn(),
    toggle: vi.fn(),
    test: vi.fn(),
  };
  vi.clearAllMocks();
  document.title = "CCCC";
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  useGroupStore.setState(previousGroup, true);
  useUIStore.setState(previousUI, true);
  useModalStore.setState(previousModal, true);
  useWorkspaceNavigation.setState({ owners: new Set(), pending: null });
});
async function mount() {
  await act(async () => root.render(<HumanAttention />));
}
async function open() {
  const trigger = host.querySelector<HTMLButtonElement>("[data-attention-trigger]")!;
  trigger.focus();
  await act(async () => trigger.click());
}
function card(id: string) {
  return document.querySelector<HTMLElement>(`[data-attention-id="${id}"]`)!;
}
it("shows three columns and counts without desktop permission, filters, and never handles actions on view", async () => {
  await mount();
  expect(document.title).toBe("(3) CCCC");
  await open();
  expect(document.querySelectorAll("[data-attention-columns] > section")).toHaveLength(3);
  expect(document.querySelectorAll("[data-attention-id]")).toHaveLength(3);
  expect(card("permission").textContent).toContain("Group Two");
  expect(card("permission").textContent).toContain("Waiting");
  expect(card("task").textContent).toContain("Time unknown");
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain(
    "Viewing is not completion",
  );
  const filter = document.querySelector<HTMLSelectElement>('select[aria-label="Filter Groups"]')!;
  await act(async () => {
    filter.value = "g-one";
    filter.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(document.querySelectorAll("[data-attention-id]")).toHaveLength(1);
  expect(openChat).not.toHaveBeenCalled();
  expect(openTask).not.toHaveBeenCalled();
  expect(select).not.toHaveBeenCalled();
});
it("navigates to the original Actor only after the target Group is hydrated", async () => {
  await mount();
  await open();
  await act(async () => card("permission").querySelector<HTMLButtonElement>("button")!.click());
  expect(select).toHaveBeenCalledWith("g-two");
  expect(useUIStore.getState().activeTab).toBe("chat");
  await act(async () => {
    useGroupStore.setState({
      actors: [{ id: "b", running: true }],
      selectedGroupActorsHydrating: false,
    });
    await new Promise((r) => setTimeout(r, 10));
  });
  expect(useUIStore.getState().activeTab).toBe("b");
  expect(openChat).not.toHaveBeenCalled();
});
it("preserves dirty editors and routes the original message only after navigation confirmation", async () => {
  useWorkspaceNavigation.setState({ owners: new Set([Symbol("fixture editor")]), pending: null });
  await mount();
  await open();
  await act(async () => card("reply").querySelector<HTMLButtonElement>("button")!.click());
  expect(select).not.toHaveBeenCalled();
  expect(openChat).not.toHaveBeenCalled();
  await act(async () => finishWorkspaceNavigation(false));
  expect(select).not.toHaveBeenCalled();
  await act(async () => card("reply").querySelector<HTMLButtonElement>("button")!.click());
  await act(async () => finishWorkspaceNavigation(true));
  await act(async () => {
    useGroupStore.setState({ selectedGroupActorsHydrating: false });
    await new Promise((r) => setTimeout(r, 10));
  });
  expect(openChat).toHaveBeenCalledWith("g-one", "event-one");
  expect(data.state.items).toHaveLength(3);
});
it("opens human-waiting tasks in the native task interface", async () => {
  await mount();
  await open();
  await act(async () => card("task").querySelector<HTMLButtonElement>("button")!.click());
  await act(async () => {
    useGroupStore.setState({ selectedGroupActorsHydrating: false });
    await new Promise((r) => setTimeout(r, 10));
  });
  expect(openTask).toHaveBeenCalledWith("T001");
  expect(openChat).not.toHaveBeenCalled();
});
it("shows stale evidence, supports keyboard close and restores the trigger focus", async () => {
  data.state.staleGroups = ["g-two"];
  await mount();
  await open();
  expect(document.querySelector('[role="alert"]')?.textContent).toContain("Group Two");
  expect(card("permission").textContent).toContain("May be stale");
  await act(async () =>
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })),
  );
  expect(document.querySelector('[role="dialog"]')).toBeNull();
  expect(document.activeElement).toBe(host.querySelector("[data-attention-trigger]"));
});

it("retires pending navigation if the human changes Group during hydration", async () => {
  await mount();
  await open();
  await act(async () => card("permission").querySelector<HTMLButtonElement>("button")!.click());
  await act(async () =>
    useGroupStore.setState({ selectedGroupId: "g-one", selectedGroupActorsHydrating: false }),
  );
  await act(async () => {
    useGroupStore.setState({
      selectedGroupId: "g-two",
      actors: [{ id: "b" }],
      selectedGroupActorsHydrating: false,
    });
    await new Promise((r) => setTimeout(r, 10));
  });
  expect(useUIStore.getState().activeTab).toBe("chat");
});
