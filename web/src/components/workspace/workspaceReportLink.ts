// The backend resolves this relative path under the authenticated Group scope.
export function workspaceReportLink(search: string) {
  const params = new URLSearchParams(search);
  const group = params.get("group");
  const file = params.get("file");
  if (!group || !file || params.has("event") || /[\\\\\x00-\x1f\x7f]/.test(file)) return null;
  if (file.split("/").some((part) => !part || part === "." || part === "..")) return null;
  return { group, file };
}

export function messageFileHref(href: string, group: string, root: string) {
  if (!group || !root || !href.startsWith("/") || /[?#]/.test(href)) return href;
  let path: string;
  try {
    path = decodeURIComponent(href);
  } catch {
    return href;
  }
  const prefix = root.replace(/\/$/, "") + "/";
  let file: string;
  if (path.startsWith(prefix)) {
    file = path.slice(prefix.length);
  } else {
    // Historical links can use the previous host's home directory.
    const project = /^\/home\/[^/]+\/Projects\/([^/]+)\/$/.exec(prefix);
    const old = /^\/home\/[^/]+\/Projects\/([^/]+)\/(.+)$/.exec(path);
    if (!project || !old || project[1] !== old[1]) return href;
    file = old[2];
  }
  const query = new URLSearchParams({ group, file }).toString();
  return workspaceReportLink(query) ? "/?" + query : href;
}
