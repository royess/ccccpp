import { expect, it } from "vite-plus/test";
import { messageFileHref, workspaceReportLink } from "./workspaceReportLink";

it("opens current and migrated file links in the authorized group workspace", () => {
  const root = "/home/current/Projects/demo";
  for (const account of ["current", "previous"]) {
    const link = messageFileHref("/home/" + account + "/Projects/demo/reports/a%20b.html", "g", root);
    expect(workspaceReportLink(link.slice(1))).toEqual({ group: "g", file: "reports/a b.html" });
  }
  for (const href of ["/home/prev/Projects/demo/../secret", "/home/prev/Projects/other/a",
    "/home/prev/Projects/demo/%2Fetc", "https://example.com/page", "/api/v1/ready"]) {
    expect(messageFileHref(href, "g", root)).toBe(href);
  }
  expect(workspaceReportLink("?group=g&file=docs%2Fa&event=x")).toBeNull();
});
