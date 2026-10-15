import type { CSSProperties, ReactNode } from "react";

const COLORS: Record<number, string> = {
  30: "#6b7280", 31: "#f87171", 32: "#4ade80", 33: "#facc15",
  34: "#60a5fa", 35: "#e879f9", 36: "#22d3ee", 37: "#d1d5db",
  90: "#9ca3af", 91: "#fca5a5", 92: "#86efac", 93: "#fde047",
  94: "#93c5fd", 95: "#f0abfc", 96: "#67e8f9", 97: "#ffffff",
};

/** 一行 ANSI 文本 → React spans（只支持 SGR 颜色/加粗） */
export function ansiLine(line: string, keyPrefix = ""): ReactNode[] {
  const out: ReactNode[] = [];
  let style: CSSProperties = {};
  let buf = "";
  let key = 0;
  const flush = () => {
    if (buf) {
      out.push(
        <span key={`${keyPrefix}${key++}`} style={style}>
          {buf}
        </span>
      );
      buf = "";
    }
  };
  const re = /\x1b\[([0-9;]*)m/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) {
    buf += line.slice(last, m.index);
    last = m.index + m[0].length;
    flush();
    const codes = m[1] ? m[1].split(";").map(Number) : [0];
    style = { ...style };
    for (const c of codes) {
      if (c === 0) style = {};
      else if (c === 1) style.fontWeight = 700;
      else if (c === 22) delete style.fontWeight;
      else if (COLORS[c]) style.color = COLORS[c];
      else if (c === 39) delete style.color;
    }
  }
  buf += line.slice(last);
  flush();
  return out;
}
