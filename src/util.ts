import { access, mkdir, readFile, stat } from "node:fs/promises";
import path from "node:path";
import { constants } from "node:fs";
import { spawn } from "node:child_process";

export type CommandResult = {
  code: number;
  stdout: string;
  stderr: string;
};

export function nowIso() {
  return new Date().toISOString();
}

export function makeJobId() {
  const stamp = Date.now().toString(36);
  const random = Math.random().toString(36).slice(2, 8);
  return `av_${stamp}_${random}`;
}

export function truncate(value: unknown, length = 96): string {
  const text = String(value || "").replace(/\s+/g, " ").trim();
  if (text.length <= length) return text;
  return `${text.slice(0, Math.max(0, length - 3)).trimEnd()}...`;
}

export function titleFromPrompt(prompt: unknown): string {
  const cleaned = String(prompt || "")
    .replace(/https?:\/\/\S+/g, "")
    .replace(/[#*_`>[\]()]/g, "")
    .replace(/\s+/g, " ")
    .trim();
  return truncate(cleaned || "untitled task", 48);
}

export function slugify(value: unknown): string {
  const slug = String(value || "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 36);
  return slug || "task";
}

export function relativeTime(iso: string | null | undefined): string {
  if (!iso) return "";
  const diffSeconds = Math.max(0, Math.floor((Date.now() - Date.parse(iso)) / 1000));
  if (diffSeconds < 60) return `${diffSeconds}s`;
  const minutes = Math.floor(diffSeconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

export function stripAnsi(value: unknown): string {
  return String(value || "").replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, "");
}

export async function pathExists(target: string): Promise<boolean> {
  try {
    await access(target, constants.F_OK);
    return true;
  } catch {
    return false;
  }
}

export async function ensureDir(target: string): Promise<void> {
  await mkdir(target, { recursive: true });
}

export function runCommand(
  command: string,
  args: string[],
  options: { cwd?: string; env?: NodeJS.ProcessEnv } = {},
): Promise<CommandResult> {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout?.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr?.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (error) => {
      resolve({ code: 127, stdout, stderr: `${stderr}${error.message}` });
    });
    child.on("close", (code) => {
      resolve({ code: code ?? 0, stdout, stderr });
    });
  });
}

export async function commandExists(command: string): Promise<boolean> {
  const result = await runCommand("sh", ["-lc", `command -v ${shellQuote(command)}`]);
  return result.code === 0;
}

export function shellQuote(value: unknown): string {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}

export type PrRef = {
  url: string;
  owner: string;
  repo: string;
  number: number;
  status: string;
};

export function extractPrRefs(text: string): PrRef[] {
  const refs: PrRef[] = [];
  const seen = new Set();
  const pattern = /https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)\/pull\/(\d+)/g;
  for (const match of String(text || "").matchAll(pattern)) {
    const url = match[0];
    if (seen.has(url)) continue;
    seen.add(url);
    refs.push({
      url,
      owner: match[1],
      repo: match[2],
      number: Number(match[3]),
      status: "unknown",
    });
  }
  return refs;
}

export function mergePrRefs(existing: PrRef[] = [], next: PrRef[] = []): PrRef[] {
  const byUrl = new Map<string, PrRef>();
  for (const ref of existing) byUrl.set(ref.url, ref);
  for (const ref of next) byUrl.set(ref.url, { ...byUrl.get(ref.url), ...ref });
  return [...byUrl.values()];
}

export function findStringByKeys(value: unknown, keys: string[]): string | null {
  const wanted = new Set(keys.map((key) => key.toLowerCase()));
  const visited = new Set();
  function visit(node: unknown): string | null {
    if (!node || typeof node !== "object") return null;
    if (visited.has(node)) return null;
    visited.add(node);
    for (const [key, child] of Object.entries(node)) {
      if (wanted.has(key.toLowerCase()) && typeof child === "string" && child.trim()) {
        return child;
      }
    }
    for (const child of Object.values(node)) {
      const found = visit(child);
      if (found) return found;
    }
    return null;
  }
  return visit(value);
}

export function collectStringsByKeys(value: unknown, keys: string[], max = 8): string[] {
  const wanted = new Set(keys.map((key) => key.toLowerCase()));
  const result: string[] = [];
  const visited = new Set();
  function visit(node: unknown): void {
    if (result.length >= max) return;
    if (!node || typeof node !== "object") return;
    if (visited.has(node)) return;
    visited.add(node);
    for (const [key, child] of Object.entries(node)) {
      if (wanted.has(key.toLowerCase()) && typeof child === "string" && child.trim()) {
        result.push(child);
        if (result.length >= max) return;
      }
    }
    for (const child of Object.values(node)) visit(child);
  }
  visit(value);
  return result;
}

export function extractThreadId(event: unknown): string | null {
  const direct = findStringByKeys(event, [
    "threadId",
    "thread_id",
    "conversationId",
    "conversation_id",
    "sessionId",
    "session_id",
    "id",
  ]);
  if (direct && looksLikeSessionId(direct)) return direct;
  const text = JSON.stringify(event);
  const uuid = text.match(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i);
  return uuid?.[0] || null;
}

export function looksLikeSessionId(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
}

export function summarizeEvent(event: unknown): string | null {
  const method = findStringByKeys(event, ["method", "type", "event", "name"]);
  const command = findStringByKeys(event, ["command", "cmd"]);
  if (command) return truncate(`Run ${command}`, 120);

  const candidates = collectStringsByKeys(event, [
    "delta",
    "text",
    "message",
    "content",
    "summary",
    "output",
    "preview",
  ]);
  const best = candidates.find((candidate) => candidate.trim().length > 2);
  if (best) return truncate(stripAnsi(best), 120);
  if (method) return truncate(method, 120);
  return null;
}

export function eventNeedsInput(event: unknown): boolean {
  const text = JSON.stringify(event).toLowerCase();
  return (
    text.includes("requestapproval") ||
    text.includes("request_approval") ||
    text.includes("requestuserinput") ||
    text.includes("request_user_input") ||
    text.includes("waitingonapproval") ||
    text.includes("waitingonuserinput") ||
    text.includes("needs_input")
  );
}

export function eventFailed(event: unknown): boolean {
  const text = JSON.stringify(event).toLowerCase();
  return text.includes('"failed"') || text.includes('"error"');
}

export async function readJsonLines(file: string): Promise<unknown[]> {
  if (!(await pathExists(file))) return [];
  const content = await readFile(file, "utf8");
  return content
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => {
      try {
        return JSON.parse(line);
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

export async function newestMtime(paths: string[]): Promise<number> {
  let latest = 0;
  for (const target of paths) {
    try {
      const info = await stat(target);
      latest = Math.max(latest, info.mtimeMs);
    } catch {
      // ignore
    }
  }
  return latest;
}

export function resolveHome(relativePath: string): string {
  return path.join(process.env.HOME || process.cwd(), relativePath);
}
