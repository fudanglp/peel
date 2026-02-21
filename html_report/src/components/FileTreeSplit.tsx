import { useState, useMemo, useEffect } from "react";
import {
  ChevronRight,
  Folder,
  ChevronsDownUp,
  ChevronsUpDown,
  ArrowUp,
  ArrowDown,
  ArrowUpDown,
} from "lucide-react";
import type { FileEntry, TreeNode } from "@/types";
import { formatBytes } from "@/lib/format";
import { buildTree } from "@/lib/tree";
import { cn } from "@/lib/utils";

type SortKey = "name" | "size";
type SortDir = "asc" | "desc";

function sortedDirChildren(node: TreeNode): TreeNode[] {
  return [...node.children.values()]
    .filter((c) => !c.isFile)
    .sort((a, b) => a.name.localeCompare(b.name));
}

function DirTreeNode({
  node,
  depth,
  expandAll,
  selectedDir,
  onSelect,
}: {
  node: TreeNode;
  depth: number;
  expandAll: boolean;
  selectedDir: TreeNode | null;
  onSelect: (node: TreeNode) => void;
}) {
  const [open, setOpen] = useState(expandAll);
  const dirs = useMemo(() => sortedDirChildren(node), [node]);
  const isSelected = selectedDir === node;

  return (
    <>
      <div
        className={cn(
          "flex items-center gap-1 py-0.5 px-2 rounded text-sm cursor-pointer",
          isSelected ? "bg-primary/10" : "hover:bg-muted/50"
        )}
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
        onClick={() => {
          onSelect(node);
          setOpen(!open);
        }}
      >
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 text-muted-foreground transition-transform",
            dirs.length === 0 && "invisible",
            open && "rotate-90"
          )}
        />
        <Folder className="size-3.5 shrink-0 text-blue-500" />
        <span className="truncate">{node.name || "/"}</span>
        <span className="ml-auto shrink-0 text-xs text-muted-foreground">
          {formatBytes(node.size)}
        </span>
      </div>
      {open &&
        dirs.map((child) => (
          <DirTreeNode
            key={child.name}
            node={child}
            depth={depth + 1}
            expandAll={expandAll}
            selectedDir={selectedDir}
            onSelect={onSelect}
          />
        ))}
    </>
  );
}

function SortIcon({ active, dir }: { active: boolean; dir: SortDir }) {
  if (!active) return <ArrowUpDown className="size-3.5" />;
  return dir === "asc" ? (
    <ArrowUp className="size-3.5" />
  ) : (
    <ArrowDown className="size-3.5" />
  );
}

export function FileTreeSplit({ files }: { files: FileEntry[] }) {
  const [expandAll, setExpandAll] = useState(false);
  const [revision, setRevision] = useState(0);
  const [selectedDir, setSelectedDir] = useState<TreeNode | null>(null);
  const [sortKey, setSortKey] = useState<SortKey>("name");
  const [sortDir, setSortDir] = useState<SortDir>("asc");

  const tree = useMemo(() => buildTree(files), [files]);

  // Reset selection when files change (layer switch)
  useEffect(() => {
    setSelectedDir(null);
  }, [files]);

  const activeDir = selectedDir ?? tree;

  const fileChildren = useMemo(() => {
    const items = [...activeDir.children.values()].filter((c) => c.isFile);
    items.sort((a, b) => {
      const cmp =
        sortKey === "name"
          ? a.name.localeCompare(b.name)
          : a.size - b.size;
      return sortDir === "asc" ? cmp : -cmp;
    });
    return items;
  }, [activeDir, sortKey, sortDir]);

  function toggleSort(key: SortKey) {
    if (sortKey === key) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortKey(key);
      setSortDir("asc");
    }
  }

  return (
    <div className="flex h-full">
      {/* Left pane: directory tree */}
      <div className="w-2/5 border-r overflow-auto p-2">
        <div className="flex items-center gap-1 mb-1 px-2">
          <button
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
            onClick={() => {
              setExpandAll(true);
              setRevision((r) => r + 1);
            }}
          >
            <ChevronsUpDown className="size-3.5" />
            Expand All
          </button>
          <span className="text-muted-foreground text-xs">/</span>
          <button
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
            onClick={() => {
              setExpandAll(false);
              setRevision((r) => r + 1);
            }}
          >
            <ChevronsDownUp className="size-3.5" />
            Collapse All
          </button>
        </div>
        {/* Root entry */}
        <div
          className={cn(
            "flex items-center gap-1 py-0.5 px-2 rounded text-sm cursor-pointer",
            selectedDir === null ? "bg-primary/10" : "hover:bg-muted/50"
          )}
          style={{ paddingLeft: "8px" }}
          onClick={() => setSelectedDir(null)}
        >
          <ChevronRight className="size-3.5 shrink-0 invisible" />
          <Folder className="size-3.5 shrink-0 text-blue-500" />
          <span className="truncate">/ root</span>
          <span className="ml-auto shrink-0 text-xs text-muted-foreground">
            {formatBytes(tree.size)}
          </span>
        </div>
        {sortedDirChildren(tree).map((child) => (
          <DirTreeNode
            key={`${child.name}-${revision}`}
            node={child}
            depth={1}
            expandAll={expandAll}
            selectedDir={selectedDir}
            onSelect={setSelectedDir}
          />
        ))}
      </div>

      {/* Right pane: file list */}
      <div className="flex-1 overflow-auto">
        <table className="w-full text-sm">
          <thead className="sticky top-0 bg-background border-b">
            <tr>
              <th className="text-left px-3 py-1.5 font-medium">
                <button
                  className="flex items-center gap-1 hover:text-foreground text-muted-foreground transition-colors"
                  onClick={() => toggleSort("name")}
                >
                  Name
                  <SortIcon active={sortKey === "name"} dir={sortDir} />
                </button>
              </th>
              <th className="text-right px-3 py-1.5 font-medium w-24">
                <button
                  className="flex items-center gap-1 ml-auto hover:text-foreground text-muted-foreground transition-colors"
                  onClick={() => toggleSort("size")}
                >
                  Size
                  <SortIcon active={sortKey === "size"} dir={sortDir} />
                </button>
              </th>
            </tr>
          </thead>
          <tbody>
            {fileChildren.length === 0 ? (
              <tr>
                <td
                  colSpan={2}
                  className="text-center text-muted-foreground py-8"
                >
                  No files in this directory
                </td>
              </tr>
            ) : (
              fileChildren.map((f) => {
                const displayName = f.is_whiteout
                  ? f.name.replace(/^\.wh\./, "")
                  : f.name;
                return (
                  <tr
                    key={f.name}
                    className="hover:bg-muted/50 border-b border-border/50"
                  >
                    <td className="px-3 py-1">
                      <span
                        className={cn(
                          f.is_whiteout && "line-through text-red-500"
                        )}
                      >
                        {displayName}
                        {f.is_whiteout && " (deleted)"}
                      </span>
                    </td>
                    <td className="text-right px-3 py-1 text-muted-foreground">
                      {formatBytes(f.size)}
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
