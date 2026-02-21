import { useMemo } from "react";
import { File } from "lucide-react";
import type { FileEntry } from "@/types";
import { formatBytes } from "@/lib/format";
import { cn } from "@/lib/utils";

export function FileSizeList({ files }: { files: FileEntry[] }) {
  const sorted = useMemo(
    () => [...files].sort((a, b) => b.size - a.size),
    [files]
  );

  const maxSize = sorted[0]?.size || 1;

  return (
    <div className="p-2">
      {sorted.map((file, i) => {
        const pct = maxSize > 0 ? (file.size / maxSize) * 100 : 0;
        return (
          <div
            key={`${file.path}-${i}`}
            className="flex items-center gap-2 py-0.5 px-2 text-sm hover:bg-muted/50 rounded relative"
          >
            <div
              className="absolute inset-y-0 left-0 bg-primary/5 rounded"
              style={{ width: `${pct}%` }}
            />
            <File className="size-3.5 shrink-0 text-muted-foreground relative" />
            <span
              className={cn(
                "truncate relative",
                file.is_whiteout && "line-through text-red-500"
              )}
            >
              {file.path}
            </span>
            <span className="ml-auto shrink-0 text-xs text-muted-foreground tabular-nums relative">
              {formatBytes(file.size)}
            </span>
          </div>
        );
      })}
    </div>
  );
}
