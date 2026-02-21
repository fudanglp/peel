import type { FileEntry, TreeNode } from "@/types";

export function buildTree(files: FileEntry[]): TreeNode {
  const root: TreeNode = {
    name: "",
    size: 0,
    is_whiteout: false,
    children: new Map(),
    isFile: false,
  };

  for (const file of files) {
    const parts = file.path.split("/");
    let current = root;

    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      const isLast = i === parts.length - 1;

      if (!current.children.has(part)) {
        current.children.set(part, {
          name: part,
          size: 0,
          is_whiteout: false,
          children: new Map(),
          isFile: false,
        });
      }

      const child = current.children.get(part)!;
      if (isLast) {
        child.size = file.size;
        child.is_whiteout = file.is_whiteout;
        child.isFile = true;
      }

      current = child;
    }
  }

  computeSizes(root);
  return root;
}

export function computeSizes(node: TreeNode): number {
  if (node.isFile) return node.size;
  let total = 0;
  for (const child of node.children.values()) {
    total += computeSizes(child);
  }
  node.size = total;
  return total;
}
